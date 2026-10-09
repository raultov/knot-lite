#!/usr/bin/env bash
# Shared helpers of the knot-lite E2E suites. Source it; do not execute it.
# Requires: bash (3.2+), jq. Linux and macOS.

E2E_PROTOCOL="2026-07-28"
E2E_REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
E2E_FIXTURES="$E2E_REPO_ROOT/tests/e2e/fixtures"
E2E_HOME="${KNOT_LITE_E2E_HOME:-$HOME/.local/share/knot-lite/e2e}"
KL="$E2E_REPO_ROOT/target/release/knot-lite"
KLM="$E2E_REPO_ROOT/target/release/knot-lite-mcp"
E2E_PASS=0; E2E_FAIL=0; E2E_SKIP=0; E2E_SUITE=""; E2E_T0=0

if [ -t 1 ]; then C_R=$'\033[0;31m'; C_G=$'\033[0;32m'; C_Y=$'\033[1;33m'; C_N=$'\033[0m'; else C_R=""; C_G=""; C_Y=""; C_N=""; fi

pass() { E2E_PASS=$((E2E_PASS + 1)); printf '%sPASS%s %s %s\n' "$C_G" "$C_N" "$1" "$2"; }
fail() { E2E_FAIL=$((E2E_FAIL + 1)); printf '%sFAIL%s %s %s\n' "$C_R" "$C_N" "$1" "$2"; shift 2; for l in "$@"; do printf '     %s\n' "$l"; done; }
skip() { E2E_SKIP=$((E2E_SKIP + 1)); printf '%sSKIP%s %s %s\n' "$C_Y" "$C_N" "$1" "$2"; }

# e2e_init <suite>: checks tools, builds (unless skipped), prepares dirs and the child env.
e2e_init() {
  E2E_SUITE="$1"; E2E_T0=$(date +%s)
  command -v jq >/dev/null 2>&1 || { echo "jq is required" >&2; exit 1; }
  if [ "${KNOT_LITE_E2E_SKIP_BUILD:-0}" != "1" ]; then
    (cd "$E2E_REPO_ROOT" && cargo build --release --bins 2>&1 | grep -E '^(error|warning: unused)|Finished' || true)
  fi
  [ -x "$KL" ] && [ -x "$KLM" ] || { echo "release binaries missing: $KL $KLM" >&2; exit 1; }
  E2E_RUN="$E2E_HOME/run/$E2E_SUITE"; E2E_WORK="$E2E_HOME/work/$E2E_SUITE"
  rm -rf "$E2E_RUN" "$E2E_WORK"
  mkdir -p "$E2E_RUN/cli" "$E2E_RUN/mcp" "$E2E_WORK" "$E2E_HOME/config" "$E2E_HOME/lsp-cache"
  export KNOT_LITE_CONFIG_DIR="$E2E_HOME/config" KNOT_LITE_LSP_CACHE_DIR="$E2E_HOME/lsp-cache"
  unset KNOT_LITE_REPO_PATH KNOT_LITE_REPO_NAME KNOT_LITE_LSP KNOT_LITE_LSP_RUST KNOT_LITE_LSP_TS KNOT_LITE_LSP_JAVA RUST_LOG
  echo "== knot-lite E2E: $E2E_SUITE =="
  trap 'e2e_on_exit' EXIT
}

# prepare_work <fixture dir name> [<work name>] → prints the work copy path.
prepare_work() {
  local src="$E2E_FIXTURES/$1" dst="$E2E_WORK/${2:-$1}"
  rm -rf "$dst"; mkdir -p "$dst"; cp -R "$src/." "$dst/"; printf '%s\n' "$dst"
}

# link_typescript <work dir>: node_modules/typescript → global install; returns 1 when unavailable.
link_typescript() {
  command -v npm >/dev/null 2>&1 || return 1
  local g; g="$(npm root -g 2>/dev/null)/typescript"; [ -d "$g" ] || return 1
  mkdir -p "$1/node_modules"; ln -s "$g" "$1/node_modules/typescript"
}

# lsp_available <rust|ts|java> → 0 available; 1 with $E2E_LSP_WHY set.
lsp_available() {
  local prog
  case "$1" in rust) prog=rust-analyzer;; ts) prog=typescript-language-server;; java) prog=jdtls;; esac
  if command -v "$prog" >/dev/null 2>&1; then return 0; fi
  E2E_LSP_WHY="$prog not found in PATH"; return 1
}

# lsp_or_skip <id> <title> <lang> → 0 run the check; 1 skipped (or failed in strict mode).
lsp_or_skip() {
  local why=""
  if ! lsp_available "$3"; then why="$E2E_LSP_WHY"
  elif [ "$3" = ts ] && [ -n "${E2E_TS_MISSING:-}" ]; then why="typescript package not resolvable"
  else return 0; fi
  if [ "${KNOT_LITE_E2E_REQUIRE_LSP:-0}" = "1" ]; then
    fail "$1" "$2" "$why (KNOT_LITE_E2E_REQUIRE_LSP=1)"
  else
    skip "$1" "$2: $why (set KNOT_LITE_E2E_REQUIRE_LSP=1 to fail)"
  fi
  return 1
}

# run_cli <id> [ENV=VAL ...] -- <knot-lite args...>: writes <id>.json/.err/.rc; prints the rc.
run_cli() {
  local id="$1"; shift; local envs=()
  while [ "$#" -gt 0 ] && [ "$1" != "--" ]; do envs+=("$1"); shift; done; shift
  local rc=0
  env ${envs[@]+"${envs[@]}"} "$KL" "$@" > "$E2E_RUN/cli/$id.json" 2> "$E2E_RUN/cli/$id.err" || rc=$?
  printf '%s\n' "$rc" > "$E2E_RUN/cli/$id.rc"; printf '%s\n' "$rc"
}

# assert_rc <id> <title> <expected> <actual>
assert_rc() { if [ "$3" = "$4" ]; then pass "$1" "$2"; else fail "$1" "$2" "expected rc $3, got $4" "files: $E2E_RUN/cli/$1.*"; fi; }

# assert_jq <id> <title> <file> <jq filter that yields true> [--arg k v ...]
assert_jq() {
  local id="$1" title="$2" file="$3" filter="$4"; shift 4
  if jq -e "$@" "$filter" "$file" >/dev/null 2>&1; then pass "$id" "$title"
  else fail "$id" "$title" "jq: $filter" "file: $file"; fi
}

# caller_set <file> [target index] → JSON array of sorted unique "file#name" (all relations).
caller_set() { jq -c --argjson i "${2:-0}" '[.targets[$i].callers[]?[]? | "\(.file)#\(.name)"] | unique' "$1"; }
# function_set: caller_set without file-level rows; file_rows: only "<file>" rows' files.
function_set() { jq -c --argjson i "${2:-0}" '[.targets[$i].callers[]?[]? | select(.name != "<file>") | "\(.file)#\(.name)"] | unique' "$1"; }
file_rows() { jq -c --argjson i "${2:-0}" '[.targets[$i].callers[]?[]? | select(.name == "<file>") | .file] | unique' "$1"; }

# assert_set_eq <id> <title> <expected JSON array> <actual JSON array>
assert_set_eq() {
  if jq -e -n --argjson e "$3" --argjson a "$4" '($e | sort) == ($a | sort)' >/dev/null; then pass "$1" "$2"
  else fail "$1" "$2" "expected: $(jq -c -n --argjson e "$3" '$e|sort')" "actual:   $(jq -c -n --argjson a "$4" '$a|sort')"; fi
}
# assert_subset <id> <title> <subset JSON> <superset JSON>
assert_subset() {
  if jq -e -n --argjson s "$3" --argjson a "$4" '($s - $a) == []' >/dev/null; then pass "$1" "$2"
  else fail "$1" "$2" "missing: $(jq -c -n --argjson s "$3" --argjson a "$4" '$s - $a')"; fi
}

# ---- MCP session over a FIFO (fd 9; bash 3.2 has no {fd} allocation) ----
# mcp_start <repo> [ENV=VAL ...]
mcp_start() {
  local repo="$1"; shift
  E2E_MCP_DIR="$E2E_RUN/mcp/s$(date +%s)$$"; mkdir -p "$E2E_MCP_DIR"
  mkfifo "$E2E_MCP_DIR/in"
  env ${@+"$@"} "$KLM" --repo-path "$repo" < "$E2E_MCP_DIR/in" > "$E2E_MCP_DIR/out.jsonl" 2> "$E2E_MCP_DIR/err.log" &
  E2E_MCP_PID=$!
  exec 9> "$E2E_MCP_DIR/in"
}
# The write runs in a subshell: a server that died would otherwise kill the suite with SIGPIPE, silently.
mcp_send() { ( printf '%s\n' "$1" >&9 ) 2>/dev/null || { echo "mcp: the server is gone (see $E2E_MCP_DIR/err.log)" >&2; return 1; }; }
mcp_meta() { jq -n -c --arg pv "$E2E_PROTOCOL" '{"io.modelcontextprotocol/protocolVersion":$pv,"io.modelcontextprotocol/clientCapabilities":{}}'; }
# mcp_request <id> <method>
mcp_request() { mcp_send "$(jq -n -c --argjson id "$1" --arg m "$2" --argjson meta "$(mcp_meta)" '{jsonrpc:"2.0",id:$id,method:$m,params:{_meta:$meta}}')"; }
# mcp_call <id> <tool> <arguments JSON>
mcp_call() { mcp_send "$(jq -n -c --argjson id "$1" --arg t "$2" --argjson a "$3" --argjson meta "$(mcp_meta)" '{jsonrpc:"2.0",id:$id,method:"tools/call",params:{name:$t,arguments:$a,_meta:$meta}}')"; }
# mcp_wait <id> <timeout s> → writes $E2E_MCP_DIR/<id>.json; returns 1 on timeout or when the server died.
mcp_wait() {
  local id="$1" limit=$(( $2 * 5 )) n=0
  while [ "$n" -lt "$limit" ]; do
    jq -c --argjson id "$id" 'select(.id == $id)' "$E2E_MCP_DIR/out.jsonl" > "$E2E_MCP_DIR/$id.json" 2>/dev/null || true
    [ -s "$E2E_MCP_DIR/$id.json" ] && return 0
    kill -0 "$E2E_MCP_PID" 2>/dev/null || return 1
    sleep 0.2; n=$((n + 1))
  done
  return 1
}
# mcp_structured <id> → path of a file holding the structuredContent of the response.
mcp_structured() { jq '.result.structuredContent' "$E2E_MCP_DIR/$1.json" > "$E2E_MCP_DIR/$1.sc.json"; printf '%s\n' "$E2E_MCP_DIR/$1.sc.json"; }
# mcp_stop [timeout s] → 0 exited by itself after EOF; 1 had to be killed.
mcp_stop() {
  exec 9>&-
  local limit=$(( ${1:-20} * 5 )) n=0
  while kill -0 "$E2E_MCP_PID" 2>/dev/null && [ "$n" -lt "$limit" ]; do sleep 0.2; n=$((n + 1)); done
  if kill -0 "$E2E_MCP_PID" 2>/dev/null; then
    kill -TERM "$E2E_MCP_PID" 2>/dev/null; sleep 2; kill -KILL "$E2E_MCP_PID" 2>/dev/null
    wait "$E2E_MCP_PID" 2>/dev/null; return 1
  fi
  wait "$E2E_MCP_PID" 2>/dev/null || true; return 0
}

# ---- hygiene ----
# proc_cwd <pid> → working directory (Linux /proc, macOS lsof).
proc_cwd() { if [ -d /proc ]; then readlink "/proc/$1/cwd" 2>/dev/null; else lsof -a -p "$1" -d cwd -Fn 2>/dev/null | sed -n 's/^n//p'; fi; }
# owned_procs → "pid name" of processes carrying an owner mark on their command line: the
# `-Dknot.lite.lsp.owner=<token>` of jdtls and the `gradle-owner-<token>` scratch path of the
# Gradle daemon. (rust-analyzer and tsserver carry the mark in their environment only.)
# The bracket keeps `pgrep -f` from matching the shell that runs it.
owned_procs() { pgrep -fl '[k]not\.lite\.lsp\.owner=|[g]radle-owner-' 2>/dev/null || true; }
# server_procs_in <dir> → pids of servers whose cwd is under <dir>.
server_procs_in() {
  local pid
  for pid in $(pgrep -f '[r]ust-analyzer|[t]ypescript-language-server|[t]sserver|[o]rg.eclipse.equinox.launcher' 2>/dev/null || true); do
    case "$(proc_cwd "$pid")" in "$1"*) printf '%s\n' "$pid";; esac
  done
}
gradle_daemons() { pgrep -f '[G]radleDaemon' 2>/dev/null | sort || true; }
e2e_baseline() { gradle_daemons > "$E2E_RUN/gradle-baseline.txt"; }
# assert_no_orphans <id> [grace s]: polls up to grace for servers under the suite's work dir,
# owner-marked processes and Gradle daemons that were not there at the baseline.
assert_no_orphans() {
  local id="$1" limit=$(( ${2:-10} * 5 )) n=0 left
  while :; do
    left="$(server_procs_in "$E2E_WORK")$(owned_procs)$(gradle_daemons | comm -13 "$E2E_RUN/gradle-baseline.txt" - 2>/dev/null)"
    [ -z "$left" ] && { pass "$id" "no orphan process"; return 0; }
    [ "$n" -ge "$limit" ] && { fail "$id" "orphan processes remain" "$left"; return 1; }
    sleep 0.2; n=$((n + 1))
  done
}
# fixtures_unchanged <id>
fixtures_unchanged() {
  local changed; changed="$(cd "$E2E_REPO_ROOT" && git status --porcelain -- tests/e2e/fixtures)"
  if [ -z "$changed" ]; then pass "$1" "fixtures untouched"; else fail "$1" "fixtures changed" "$changed"; fi
}

e2e_on_exit() {
  # Never leave an MCP server behind, even when the suite aborts.
  if [ -n "${E2E_MCP_PID:-}" ] && kill -0 "$E2E_MCP_PID" 2>/dev/null; then kill -KILL "$E2E_MCP_PID" 2>/dev/null || true; fi
}

# e2e_finish → summary line; exit status 0/1. Deletes the work copy when KNOT_LITE_E2E_KEEP=0 and green.
e2e_finish() {
  local secs=$(( $(date +%s) - E2E_T0 ))
  printf -- '-- %s: %d passed, %d failed, %d skipped (%d s)\n' "$E2E_SUITE" "$E2E_PASS" "$E2E_FAIL" "$E2E_SKIP" "$secs"
  if [ "$E2E_FAIL" -eq 0 ] && [ "${KNOT_LITE_E2E_KEEP:-1}" = "0" ]; then rm -rf "$E2E_WORK"; fi
  [ "$E2E_FAIL" -eq 0 ]
}
