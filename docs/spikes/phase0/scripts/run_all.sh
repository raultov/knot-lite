#!/usr/bin/env bash
# T10/T11 — reproduce every Phase 0 measurement end to end.
#
# Expected wall time on the reference machine (32 cores): ~3 h, dominated by the
# spring-ai S2 index (~22 min) and determinism (~88 min: 4 index runs).
#
# Prerequisites (see README.md): jdtls 1.61.0, rust-analyzer, tsserver, knot
# CLI + running Qdrant/Neo4j, python3 3.11+, the sandbox copies under
# /tmp/knot-rag-spike/work, and the fastembed cache. The gold step needs the
# adjudication files (committed in results/adjudication/) — re-creating them is
# manual by design (T7.3: a human/agent reads the code).
set -euo pipefail
source "$(dirname "$0")/env.sh"
CRATE="$(cd "$(dirname "$0")/.." && pwd)"
REPO_ROOT="$(cd "$CRATE/../../.." && pwd)"
cd "$CRATE"
BIN=target/debug

echo "== 0. quality gates (~1 min) =="
make -C "$REPO_ROOT" check

echo "== 1. sandbox + ground truth (~2 min) =="
bash scripts/setup_sandbox.sh >/dev/null
bash scripts/groundtruth.sh >/dev/null

echo "== 2. validate targets (~2 min: parses the three snapshots) =="
for spec in "rust_knot rust" "ts_knot_site ts" "java_spring_ai java"; do
  toml="${spec%% *}"; lang="${spec##* }"
  "$BIN/lsp_probe" validate-targets --targets "targets/$toml.toml" >/dev/null
  echo "  $lang ok"
done

echo "== 3. S1 matrix (~12 min; java cold is the long one) =="
for spec in "rust_knot rust" "ts_knot_site ts" "java_spring_ai java"; do
  toml="${spec%% *}"; lang="${spec##* }"
  for run in cold warm; do
    "$BIN/lsp_probe" run --targets "targets/$toml.toml" --run "$run" \
      --out "results/s1_${lang}_${run}.json" >/dev/null
    echo "  $lang $run"
  done
done

echo "== 4. degradation (T8, ~3 min) =="
for spec in "rust_knot rust" "ts_knot_site ts" "java_spring_ai java"; do
  toml="${spec%% *}"; lang="${spec##* }"
  "$BIN/lsp_probe" nolsp --targets "targets/$toml.toml" \
    --out "results/s1_${lang}_nolsp.json" >/dev/null
  "$BIN/lsp_probe" nolsp --targets "targets/$toml.toml" --program /bin/false \
    --out "results/s1_${lang}_nolsp_broken.json" >/dev/null
  echo "  $lang"
done

echo "== 5. heuristic graph (T6, ~1 min) =="
for spec in "rust_knot rust" "ts_knot_site ts" "java_spring_ai java"; do
  toml="${spec%% *}"; lang="${spec##* }"
  "$BIN/lsp_probe" heuristic --targets "targets/$toml.toml" \
    --out "results/heuristic_${lang}.json" >/dev/null
  echo "  $lang"
done

echo "== 6. gold metrics (T7, ~2 min; adjudication files are inputs) =="
for spec in "rust_knot rust" "ts_knot_site ts" "java_spring_ai java"; do
  toml="${spec%% *}"; lang="${spec##* }"
  "$BIN/lsp_probe" gold --lang "$lang" --targets "targets/$toml.toml" >/dev/null
  echo "  $lang"
done

echo "== 7. S2 index + measure (~35 min; spring-ai index ~22 min) =="
for spec in "knot knot" "knot-site knot-site" "spring-ai spring-ai"; do
  repo="${spec%% *}"; dir="${spec##* }"
  "$BIN/vec_probe" index --repo "$repo" --root "$SPIKE_ROOT/work/$dir" \
    --out-dir "$SPIKE_ROOT/s2/$repo" >/dev/null
  "$BIN/vec_probe" measure --repo "$repo" --root "$SPIKE_ROOT/work/$dir" \
    --out-dir "$SPIKE_ROOT/s2/$repo" --queries queries/entities.toml \
    --level1 queries/level1.toml --result "results/s2_${repo}.json" >/dev/null
  echo "  $repo"
done

echo "== 8. determinism (T9.7, ~95 min; 4 index runs per repo) =="
for spec in "knot knot" "knot-site knot-site" "spring-ai spring-ai"; do
  repo="${spec%% *}"; dir="${spec##* }"
  RAYON_NUM_THREADS=1 "$BIN/vec_probe" index --repo "$repo" \
    --root "$SPIKE_ROOT/work/$dir" --out-dir "$SPIKE_ROOT/s2/$repo/det-threads" >/dev/null
  "$BIN/vec_probe" determinism --repo "$repo" --root "$SPIKE_ROOT/work/$dir" \
    --out-dir "$SPIKE_ROOT/s2/$repo" --result "results/s2_${repo}.json" \
    --threads-dir "$SPIKE_ROOT/s2/$repo/det-threads" >/dev/null
  echo "  $repo"
done

echo "== 9. git pack + churn (T9.6, ~10 min) =="
bash scripts/gitmeasure.sh knot "$SPIKE_ROOT/work/knot" "$SPIKE_ROOT/s2/knot" results/s2_knot.json >/dev/null
bash scripts/gitmeasure.sh knot-site "$SPIKE_ROOT/work/knot-site" "$SPIKE_ROOT/s2/knot-site" results/s2_knot-site.json >/dev/null
bash scripts/gitmeasure.sh spring-ai "$SPIKE_ROOT/work/spring-ai" "$SPIKE_ROOT/s2/spring-ai" results/s2_spring-ai.json >/dev/null

echo "== 10. Qdrant parity (T9.9, ~1 min) =="
for repo in knot knot-site spring-ai; do
  bash scripts/qdrant_dump.sh "$repo" "$SPIKE_ROOT/s2/$repo" "/tmp/qdrant_$repo.json" >/dev/null
  "$BIN/vec_probe" parity --repo "$repo" --out-dir "$SPIKE_ROOT/s2/$repo" \
    --qdrant-dump "/tmp/qdrant_$repo.json" --result "results/s2_${repo}.json" >/dev/null
done

echo "== 11. report (~1 s) =="
"$BIN/summarize" --results results --out ../phase0-results.md --template-check
"$BIN/summarize" --results results --out ../phase0-results.md

echo "done — see docs/spikes/phase0-results.md"
