#!/usr/bin/env bash
# knot CLI wrapper: pins the embedding model/dimension used by the live index.
set -euo pipefail
source "$(dirname "$0")/env.sh"
cd /tmp   # avoid picking up any project-local .env
exec knot "$@"
