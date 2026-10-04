#!/usr/bin/env bash
# T2.2 — fetch knot ground truth for every S1 target and every tier-3 query.
#
# Outputs (raw knot JSON, committed):
#   groundtruth/callers_<repo>_<id>.json   per [[target]] of targets/*.toml
#   groundtruth/search_<repo>_<qid>.json   per [[query]] of queries/entities.toml
#
# Any failure (non-zero exit, empty output) aborts naming the offending id.
set -euo pipefail
source "$(dirname "$0")/env.sh"
CRATE="$(cd "$(dirname "$0")/.." && pwd)"
cd "$CRATE"
mkdir -p groundtruth

KNOT="$CRATE/scripts/knot.sh"
PY_TOML='import tomllib, pathlib, sys
sep = "\t"
for tf in sorted(pathlib.Path(sys.argv[1]).glob("*.toml")):
    data = tomllib.loads(tf.read_text())
    key = sys.argv[2]
    for item in data.get(key, []):
        if key == "target":
            print(data["repo"], item["id"], item["knot_query"], sep=sep)
        else:
            print(item["repo"], item["id"], item["text"], sep=sep)
'

echo "== knot callers per target =="
while IFS=$'\t' read -r repo tid query; do
  out="groundtruth/callers_${repo}_${tid}.json"
  if ! "$KNOT" callers "$query" -r "$repo" -m 500 -k all -o json > "$out"; then
    echo "ERROR: knot callers failed for target ${tid} (query: ${query})" >&2
    exit 1
  fi
  if [ ! -s "$out" ]; then
    echo "ERROR: empty knot callers output for target ${tid} (query: ${query})" >&2
    exit 1
  fi
  echo "  ${tid} -> ${out}"
done < <(python3 -c "$PY_TOML" targets target)

echo "== knot search per tier-3 query =="
while IFS=$'\t' read -r repo qid text; do
  out="groundtruth/search_${repo}_${qid}.json"
  if ! "$KNOT" search "$text" -r "$repo" -m 10 -o json > "$out"; then
    echo "ERROR: knot search failed for query ${qid} (text: ${text})" >&2
    exit 1
  fi
  if [ ! -s "$out" ]; then
    echo "ERROR: empty knot search output for query ${qid} (text: ${text})" >&2
    exit 1
  fi
  echo "  ${qid} -> ${out}"
done < <(python3 -c "$PY_TOML" queries query)

echo "ground truth written to $CRATE/groundtruth/"
