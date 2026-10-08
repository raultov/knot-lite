#!/bin/bash
# Shared helper functions and environment setup for knot-lite E2E test suites.
set -euo pipefail

export KL_HOME="${KNOT_LITE_MEASURE_HOME:-$HOME/.local/share/knot-lite}"
export KL="${KL_BIN:-$PWD/target/release/knot-lite}"
export KLM="${KLM_BIN:-$PWD/target/release/knot-lite-mcp}"
export E2E_CACHE_DIR="${KNOT_LITE_LSP_CACHE_DIR:-$KL_HOME/lsp-cache}"
export E2E_CONFIG_DIR="${KNOT_LITE_CONFIG_DIR:-$KL_HOME/e2e/config}"

mkdir -p "$E2E_CACHE_DIR" "$E2E_CONFIG_DIR"

e2e_init_repo() {
  local repo_path="$1"
  shift
  mkdir -p "$repo_path"
  if [ ! -d "$repo_path/.git" ]; then
    git -C "$repo_path" init -q
    git -C "$repo_path" config user.name "E2E Test"
    git -C "$repo_path" config user.email "e2e@test.local"
  fi
}

e2e_index_repo() {
  local repo_path="$1"
  shift
  "$KL" --repo-path "$repo_path" index --lsp-precompute none "$@"
}

e2e_assert_json_field() {
  local json_file="$1"
  local jq_expr="$2"
  local expected="$3"
  local actual
  actual=$(jq -r "$jq_expr" "$json_file")
  if [ "$actual" != "$expected" ]; then
    echo "E2E ASSERTION FAILED in $json_file: expected '$expected', got '$actual' (expr: $jq_expr)" >&2
    return 1
  fi
}

e2e_assert_live() {
  local json_file="$1"
  e2e_assert_json_field "$json_file" '.provenance // .structuredContent.provenance // "none"' "lsp-live"
}
