#!/usr/bin/env bash
# T9.6 — git pack measurement: source tree, every shard variant and monolith,
# then the append/prepend incremental churn experiment. Writes
# results/s2_<repo>.git.json (merged into the S2 result by `vec_probe measure`
# consumers / the report).
#
# Usage: gitmeasure.sh <repo> <root> <s2-out-dir> <results/s2_repo.json>
set -euo pipefail
source "$(dirname "$0")/env.sh"
CRATE="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$CRATE/target/debug/vec_probe"
REPO="$1"; ROOT="$2"; S2="$3"; RESULT="$4"
GITROOT="$SPIKE_ROOT/gitmeasure/$REPO"
VARIANTS="f32_dec f32_b64 i8pv_b64 i8fx_b64"
case "$REPO" in
  knot)      CHURN_FILE="src/pipeline/files.rs" ;;
  knot-site) CHURN_FILE="src/toolcore/format.ts" ;;
  spring-ai) CHURN_FILE="spring-ai-commons/src/main/java/org/springframework/ai/document/Document.java" ;;
  *) echo "unknown repo $REPO" >&2; exit 1 ;;
esac

pack_of() { git -C "$1" count-objects -v | awk '/^size-pack:/ {print $2}'; }

commit_tree() { # $1 = git dir, $2 = content dir
  rm -rf "$1"; mkdir -p "$1"
  git -C "$1" init -q
  cp -a "$2/." "$1/"
  git -C "$1" add -A
  git -C "$1" -c user.name=spike -c user.email=spike@localhost commit -qm base
  git -C "$1" gc --aggressive --prune=now -q
}

echo "== git pack: $REPO =="
rm -rf "$GITROOT"; mkdir -p "$GITROOT/pristine"
LOG="$GITROOT/measure.txt"; : > "$LOG"
declare -A PACK
commit_tree "$GITROOT/source" "$ROOT"
PACK[source]=$(pack_of "$GITROOT/source")
echo "source ${PACK[source]}" | tee -a "$LOG"

for v in $VARIANTS; do
  for suffix in "" "-monolith"; do
    name="$v$suffix"
    [ -d "$S2/$name" ] || continue
    cp -a "$S2/$name" "$GITROOT/pristine/$name"
    commit_tree "$GITROOT/$name" "$GITROOT/pristine/$name"
    PACK[$name]=$(pack_of "$GITROOT/$name")
    echo "$name ${PACK[$name]}" | tee -a "$LOG"
  done
done

echo "== churn ($CHURN_FILE) =="
{
for mode in append prepend; do
  "$BIN" mutate --root "$ROOT" --file "$CHURN_FILE" --mode "$mode" >/dev/null
  "$BIN" index --repo "$REPO" --root "$ROOT" --out-dir "$S2" --per-file-batches \
    --only-file "$CHURN_FILE" >/dev/null
  for v in $VARIANTS; do
    for suffix in "" "-monolith"; do
      name="$v$suffix"
      [ -d "$S2/$name" ] || continue
      D="$GITROOT/churn-$mode-$name"
      rm -rf "$D"; mkdir -p "$D"
      git -C "$D" init -q
      cp -a "$GITROOT/pristine/$name/." "$D/"
      git -C "$D" add -A
      git -C "$D" -c user.name=spike -c user.email=spike@localhost commit -qm base
      cp -a "$S2/$name/." "$D/"
      git -C "$D" add -A
      git -C "$D" -c user.name=spike -c user.email=spike@localhost commit -qm "$mode" >/dev/null
      git -C "$D" gc --prune=now -q
      delta=$(( $(pack_of "$D") - ${PACK[$name]:-0} ))
      stat=$(git -C "$D" diff --shortstat HEAD~1 HEAD 2>/dev/null | tr -d '\n' || true)
      files=$(printf '%s' "$stat" | sed -n 's/.*\([0-9][0-9]*\) files\{0,1\} changed.*/\1/p')
      lines=$(printf '%s' "$stat" | sed -n 's/.*, \([0-9][0-9]*\) insertions\{0,1\}.*/\1/p')
      echo "$mode $name ${PACK[$name]} $delta ${files:-1} ${lines:-0}"
    done
  done
  bash "$CRATE/scripts/setup_sandbox.sh" >/dev/null 2>&1 || true
done
} | tee -a "$LOG"

python3 - "$RESULT" "$GITROOT/measure.txt" "$REPO" <<'PY'
import json, sys, pathlib
result_path, log_path, repo = sys.argv[1], sys.argv[2], sys.argv[3]
pack, ratio, churn = {}, {}, {"append": {}, "prepend": {}}
for line in pathlib.Path(log_path).read_text().splitlines():
    parts = line.split()
    if not parts:
        continue
    if parts[0] in ("source",) or (len(parts) == 2 and parts[1].isdigit()):
        pack[parts[0]] = int(parts[1])
        continue
    if len(parts) == 6 and parts[0] in ("append", "prepend"):
        mode, name, base, delta, files, lines = parts
        churn[mode][name] = {
            "pack_delta_kib": int(delta),
            "files_changed": int(files),
            "lines_changed": int(lines),
        }
source = pack.get("source", 0)
for name, kib in pack.items():
    if name != "source" and source:
        ratio[name] = round(kib / source, 4)
result_path = pathlib.Path(result_path)
doc = json.loads(result_path.read_text()) if result_path.exists() else {}
doc["git"] = {"pack_kib": pack, "ratio_vs_source": ratio, "churn": churn}
result_path.write_text(json.dumps(doc, indent=2) + "\n")
print(f"merged git metrics into {result_path}")
PY
