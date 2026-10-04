#!/usr/bin/env bash
# T9.9 — dump 200 deterministically sampled spike vectors from knot's Qdrant.
#
# Usage: qdrant_dump.sh <repo> <s2-out-dir> <out.json>
set -euo pipefail
source "$(dirname "$0")/env.sh"
CRATE="$(cd "$(dirname "$0")/.." && pwd)"
REPO="$1"; S2="$2"; OUT="$3"
IDS="$S2/parity_ids.json"

"$CRATE/target/debug/vec_probe" parity --repo "$REPO" --out-dir "$S2" \
  --qdrant-dump /dev/null --result /dev/null --ids-out "$IDS" >/dev/null

# knot's collection uses numeric point ids with the entity UUID in the
# `uuid` payload field — retrieve by filter, not by id.
curl -fsS -X POST "http://localhost:6333/collections/knot_entities/points/scroll" \
  -H 'Content-Type: application/json' \
  -d "{\"filter\": {\"must\": [{\"key\": \"uuid\", \"match\": {\"any\": $(cat "$IDS")}}]}, \"limit\": 200, \"with_vector\": true, \"with_payload\": true}" > "$OUT"
python3 -c "import json;d=json.load(open('$OUT'));print('wrote $OUT', len(d['result']['points']), 'points')"
