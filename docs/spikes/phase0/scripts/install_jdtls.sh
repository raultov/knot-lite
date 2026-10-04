#!/usr/bin/env bash
set -euo pipefail
V=1.61.0
F=jdt-language-server-1.61.0-202609031315.tar.gz
BASE=https://download.eclipse.org/jdtls/milestones/$V
DEST="$HOME/.local/share/jdtls/$V"
if [ -x "$HOME/.local/bin/jdtls" ] && [ -d "$DEST/plugins" ]; then
  echo "jdtls $V already installed"; exit 0
fi
TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
cd "$TMP"
curl -fsSLO "$BASE/$F"
curl -fsSLO "$BASE/$F.sha256"
# The .sha256 file may contain just the hash, or "hash  filename".
HASH=$(awk '{print $1}' "$F.sha256")
echo "$HASH  $F" | sha256sum -c -
mkdir -p "$DEST"
tar xzf "$F" -C "$DEST"
ln -sfn "$DEST" "$HOME/.local/share/jdtls/current"
mkdir -p "$HOME/.local/bin"
ln -sf "$HOME/.local/share/jdtls/current/bin/jdtls" "$HOME/.local/bin/jdtls"
"$HOME/.local/bin/jdtls" --help >/dev/null 2>&1 || true
echo "installed jdtls $V into $DEST"
