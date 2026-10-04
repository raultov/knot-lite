#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "$0")/env.sh"
mkdir -p "$SPIKE_ROOT"/{work,target-ra,jdtls-ws,s2,gitmeasure,logs}
for r in knot knot-site spring-ai; do
  rsync -a --delete \
    --exclude '.knot/' --exclude '.fastembed_cache/' --exclude 'target/' \
    --exclude 'node_modules/' \
    "$HOME/.knot/repos/$r/" "$SPIKE_ROOT/work/$r/"
done
# tsserver needs dependency types; link (do NOT copy) the real node_modules.
ln -sfn "$HOME/workspace/typescript/knot-site/node_modules" "$SPIKE_ROOT/work/knot-site/node_modules"
# Fingerprint the snapshots so the report can cite exactly what was measured.
for r in knot knot-site spring-ai; do
  ( cd "$SPIKE_ROOT/work/$r" && find . -type f -not -path './node_modules/*' -print0 \
      | sort -z | xargs -0 sha256sum | sha256sum | awk -v r="$r" '{print r, $1}' )
done > "$SPIKE_ROOT/work/FINGERPRINTS.txt"
cat "$SPIKE_ROOT/work/FINGERPRINTS.txt"
