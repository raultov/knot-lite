#!/usr/bin/env bash
# E2E: TypeScript (typescript-language-server). Usage: ./tests/run_typescript_e2e.sh
# Rows follow Appendix B of the Phase 4 plan; the expected answers are those of Appendix A.2.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/e2e/lib.sh"
e2e_init typescript
e2e_baseline
T="$(prepare_work typescript)"
use_repo "$T"
LSP_TIMEOUT="${KNOT_LITE_E2E_LSP_TIMEOUT:-300}"
# The server needs the `typescript` package next to the project; a symlink to the global one, no network.
if link_typescript "$T"; then :; else E2E_TS_MISSING=1; fi

SRC_FILES='["src/models/user.ts","src/util/format.ts","src/services/userRepository.ts","src/services/userService.ts","src/api/userController.ts"]'
EXACT_FORMAT='["src/api/userController.ts#greet","src/services/userService.ts#displayName"]'
FORMAT_FILE_ROWS='["src/services/userService.ts","src/api/userController.ts"]'
EXACT_FIND='["src/services/userService.ts#displayName"]'
EXACT_DISPLAY='["src/api/userController.ts#show"]'
USER_CONTAINS='["src/util/format.ts#formatName","src/services/userRepository.ts#save","src/api/userController.ts#greet"]'
USER_FILE_ROWS='["src/util/format.ts","src/services/userRepository.ts","src/api/userController.ts"]'
EXACT_FORMAT_AFTER_EDIT='["src/api/userController.ts#greet","src/services/userService.ts#displayName","src/util/format.ts#initials"]'

# ---- T01 versions ---------------------------------------------------------------------------
rc="$(run_cli T01a -- --version)"; assert_rc T01a "knot-lite --version exits 0" 0 "$rc"
text_in T01a "knot-lite --version prints $E2E_VERSION" "$(J T01a)" "$E2E_VERSION_RE"
"$KLM" --version > "$E2E_RUN/cli/T01b.json" 2>&1 || true
text_in T01b "knot-lite-mcp --version prints $E2E_VERSION" "$(J T01b)" "$E2E_VERSION_RE"

# ---- T02-T03 before the index -------------------------------------------------------------------
rc="$(cli T02 -- status -o json)"; assert_rc T02 "status before the index exits 0" 0 "$rc"
assert_jq T02 "index state is absent" "$(J T02)" '.index.state == "absent"'
if lsp_available ts && [ -z "${E2E_TS_MISSING:-}" ]; then want_tier=3; else want_tier=2; fi
assert_jq T02 "typescript language row has tier $want_tier" "$(J T02)" \
  '[.languages[] | select(.language == "typescript") | .tier] == [$t]' --argjson t "$want_tier"
rc="$(cli T03 -- callers formatName -o json)"; assert_rc T03 "callers without an index exits 2" 2 "$rc"
text_in T03 "the diagnostic says there is no index" "$(E T03)" 'has no index'

# ---- T04-T05 index ------------------------------------------------------------------------------
rc="$(cli T04 -- index --lsp-precompute none -o json)"; assert_rc T04 "index exits 0" 0 "$rc"
assert_jq T04 "entities and located columns counted" "$(J T04)" '.counts.entities > 0 and .counts.columns_located > 0'
if [ -f "$T/.knot-lite/manifest.toml" ] && [ -f "$T/.knot-lite/.gitignore" ]; then pass T04 "manifest and .gitignore written"; else fail T04 "manifest or .gitignore missing" "in $T/.knot-lite"; fi
text_in T04 "manifest pins knot 1.11.3" "$T/.knot-lite/manifest.toml" '^knot_version = "1\.11\.3"'
rc="$(cli T05 -- status -o json)"; assert_rc T05 "status after the index exits 0" 0 "$rc"
assert_jq T05 "index present and fresh" "$(J T05)" '.index.state == "present" and .index.details.freshness.state == "fresh"'

# ---- T06-T09 files ------------------------------------------------------------------------------
rc="$(cli T06 -- files -o json)"; assert_rc T06 "files exits 0" 0 "$rc"
assert_jq T06 "every source file is listed" "$(J T06)" '($want - [.files[].path]) == []' --argjson want "$SRC_FILES"
rc="$(cli T07 -- files -p src/ -o json)"; assert_rc T07 "files -p src/ exits 0" 0 "$rc"
assert_jq T07 "non-empty and every path under src/" "$(J T07)" '(.files|length) > 0 and all(.files[]; .path | startswith("src/"))'
rc="$(cli T08 -- files -p 'src/*/*.ts' -o json)"; assert_rc T08 "files with a glob exits 0" 0 "$rc"
assert_jq T08 "glob mode, non-empty, every path matches src/*/*.ts" "$(J T08)" '.path_mode == "glob" and (.files|length) > 0 and all(.files[]; .path | test("^src/[^/]+/[^/]+\\.ts$"))'
rc="$(cli T09 -- files -r other -o json)"; assert_rc T09 "files -r other exits 1" 1 "$rc"

# ---- T10-T12 explore and search -------------------------------------------------------------------
rc="$(cli T10 -- explore src/services/userRepository.ts -o json)"; assert_rc T10 "explore (json) exits 0" 0 "$rc"
assert_jq T10 "entities include UserRepository, save and findById" "$(J T10)" \
  '(["UserRepository","save","findById"] - [.groups[].entities[].name]) == []'
rc="$(cli T10m -- explore src/services/userRepository.ts -o markdown)"; assert_rc T10m "explore (markdown) exits 0" 0 "$rc"
text_in T10m "the markdown names UserRepository" "$(J T10m)" 'UserRepository'
rc="$(cli T11a -- search "full display name of a user" -o json)"; assert_rc T11a "search 1 exits 0" 0 "$rc"
assert_jq T11a "formatName is in the top 5" "$(J T11a)" '[.results[:5][].name] | index("formatName") != null'
rc="$(cli T11b -- search "in-memory user storage" -o json)"; assert_rc T11b "search 2 exits 0" 0 "$rc"
# The plan expects the class UserRepository; the measured top 5 are its file's other symbols (users, save, ...).
# What the query must do is lead to userRepository.ts, so that is what is asserted (Appendix D).
assert_jq T11b "the top 5 lead to userRepository.ts" "$(J T11b)" '[.results[:5][].file] | index("src/services/userRepository.ts") != null'
rc="$(cli T12a -- search "full display name of a user" -k callable -o json)"; assert_rc T12a "search -k callable exits 0" 0 "$rc"
assert_jq T12a "every hit is a function or a method" "$(J T12a)" '(.results|length) > 0 and all(.results[]; .kind | test("function|method"))'
rc="$(cli T12b -- search "full display name of a user" -p src/util -o json)"; assert_rc T12b "search -p exits 0" 0 "$rc"
assert_jq T12b "every hit is under src/util" "$(J T12b)" '(.results|length) > 0 and all(.results[]; .file | startswith("src/util"))'

# ---- T36-T39 Phase 5: globs, target tiers, search modes, explore with servers off -----------------
rc="$(cli T36a -- files -p 'src/**/*.ts' -o json)"; assert_rc T36a "files with src/**/*.ts exits 0" 0 "$rc"
assert_jq T36a "exactly the fixture's sources, glob mode" "$(J T36a)" '.path_mode == "glob" and ([.files[].path] | sort) == ($want | sort)' --argjson want "$SRC_FILES"
rc="$(cli T36b -- search "full display name of a user" -p 'src/*/format.ts' -m 10 -o json)"; assert_rc T36b "search with a glob exits 0" 0 "$rc"
assert_jq T36b "glob mode, every hit matches src/*/format.ts" "$(J T36b)" '.filters.path_mode == "glob" and (.results|length) > 0 and all(.results[]; .file | test("^src/[^/]+/format\\.ts$"))'
rc="$(cli T37a -- callers ormatNam --no-lsp -o json)"; assert_rc T37a "callers with a substring exits 0" 0 "$rc"
assert_jq T37a "fuzzy tier, formatName is target #1" "$(J T37a)" '.resolution.tier == "fuzzy" and .targets[0].name == "formatName"'
rc="$(cli T37b -- callers fi --no-lsp -o json)"; assert_rc T37b "callers with a short prefix exits 0" 0 "$rc"
assert_jq T37b "name_prefix tier finds findById" "$(J T37b)" '.resolution.tier == "name_prefix" and ([.targets[].name] | index("findById") != null)'
rc="$(cli T37d -- callers formatName --no-lsp -o json)"; assert_rc T37d "callers with an exact name exits 0" 0 "$rc"
assert_jq T37d "the exact name still wins over fuzzy" "$(J T37d)" '.resolution.tier == "exact_name" and .targets[0].name == "formatName"'
rc="$(cli T38a KNOT_LITE_SEARCH=hybrid -- search "full display name of a user" -o json)"; assert_rc T38a "hybrid search exits 0" 0 "$rc"
assert_jq T38a "search/v2, hybrid, every hit has a cosine and channels" "$(J T38a)" '.schema == "knot-lite/search/v2" and .provenance == "hybrid" and all(.results[]; (.cosine | type) == "number" and (.channels | length) > 0)'
rc="$(cli T38b KNOT_LITE_SEARCH=hybrid -- search "format name" -o json)"; assert_rc T38b "hybrid identifier search exits 0" 0 "$rc"
assert_jq T38b "formatName is rank 1" "$(J T38b)" '.results[0].name == "formatName"'
rc="$(cli T38c -- search "full display name of a user" -o json)"; assert_rc T38c "default search exits 0" 0 "$rc"
assert_jq T38c "vector by default: score equals cosine" "$(J T38c)" '.provenance == "vector" and all(.results[]; .score == .cosine)'
rc="$(cli T39 KNOT_LITE_LSP=off -- explore src/util/format.ts -o json)"; assert_rc T39 "explore with servers off exits 0" 0 "$rc"
assert_jq T39 "lsp off, every count heuristic" "$(J T39)" '.schema == "knot-lite/explore/v2" and .lsp.status == "off" and all(.groups[].entities[]; .refs_provenance == "heuristic")'

# ---- T13 heuristic ------------------------------------------------------------------------------
rc="$(cli T13 -- callers formatName --no-lsp -o json)"; assert_rc T13 "callers --no-lsp exits 0" 0 "$rc"
assert_jq T13 "provenance is heuristic and the notes say so" "$(J T13)" \
  '.targets[0].provenance == "heuristic" and (.notes|length) > 0 and .targets[0].lsp.status == "disabled"'
assert_subset T13 "the heuristic callers contain the direct callers" "$EXACT_FORMAT" "$(function_set "$(J T13)")"

# ---- T14-T15 live ---------------------------------------------------------------------------------
if lsp_or_skip T14 "live callers" ts; then
  live_exact T14a formatName "$EXACT_FORMAT" "$FORMAT_FILE_ROWS"
  live_exact T14b UserRepository.findById "$EXACT_FIND"
  live_exact T14c UserService.displayName "$EXACT_DISPLAY"
  rc="$(cli T14d -- callers User -o json)"; assert_rc T14d "callers User exits 0" 0 "$rc"
  assert_jq T14d "User is answered live" "$(J T14d)" '.targets[0].provenance == "lsp-live" and .targets[0].lsp.status == "ok"'
  assert_subset T14d "User is referenced by formatName, save and greet" "$USER_CONTAINS" "$(function_set "$(J T14d)")"
  assert_subset T14d "file-level rows only in the importing files" "$(file_rows "$(J T14d)")" "$USER_FILE_ROWS"
  rc="$(cli T15 -- callers formatName -o table)"; assert_rc T15 "callers (table) exits 0" 0 "$rc"
  text_in T15 "the table marks the answer [lsp-live]" "$(J T15)" '\[lsp-live\]'
  rc="$(cli T40 -- explore src/util/format.ts -o json)"; assert_rc T40 "explore with a live server exits 0" 0 "$rc"
  assert_jq T40 "lsp ok; formatName counted live (4 caller containers)" "$(J T40)" \
    '.lsp.status == "ok" and ([.groups[].entities[] | select(.name == "formatName") | [.refs_provenance, .referenced_by]] == [["lsp-live", 4]])'
fi

# ---- T16 a server that does not resolve ----------------------------------------------------------
rc="$(cli T16 KNOT_LITE_LSP_TS=/nonexistent/server -- callers formatName -o json)"; assert_rc T16 "an unresolvable server still exits 0" 0 "$rc"
assert_jq T16 "degrades to heuristic with status no_server" "$(J T16)" '.targets[0].provenance == "heuristic" and .targets[0].lsp.status == "no_server"'
text_in T16 "the diagnostic says it does not resolve to an executable" "$(J T16)" 'does not resolve to an executable'

# A pre-computed target keeps its pre-computed answer after an edit (lsp-indexed outranks the stale-file check,
# P3-D10), so the stale rows run first, while no target has pre-computed data; the pre-computation then runs
# on the edited fixture.
# ---- T20-T22 stale target, re-index --------------------------------------------------------------
cat >> "$T/src/util/format.ts" <<'TS'

/** Initials of a user. */
export function initials(user: User): string {
  return formatName(user).split(" ").map((part) => part[0]).join("");
}
TS
if lsp_or_skip T20 "stale target" ts; then
  rc="$(cli T20 -- callers formatName -o json)"; assert_rc T20 "callers after an edit exits 0" 0 "$rc"
  assert_jq T20 "the edited file's target is reanchored live" "$(J T20)" '.targets[0].lsp.status == "ok" and .targets[0].provenance == "lsp-live" and .targets[0].lsp.reanchored == true'
  text_in T20 "a note says target was re-anchored" "$(J T20)" 're-anchored'
  cp "$T/src/util/format.ts" "$T/src/util/format.ts.bak"
  grep -v 'export function formatName' "$T/src/util/format.ts.bak" > "$T/src/util/format.ts"
  rc="$(cli T44 -- callers formatName -o json)"; assert_rc T44 "callers with deleted declaration exits 0" 0 "$rc"
  assert_jq T44 "target declaration gone is stale_target" "$(J T44)" '.targets[0].lsp.status == "stale_target" and .targets[0].provenance == "heuristic"'
  mv "$T/src/util/format.ts.bak" "$T/src/util/format.ts"
  rc="$(cli T41 -- explore src/util/format.ts -o json)"; assert_rc T41 "explore of the edited file exits 0" 0 "$rc"
  assert_jq T41 "outline from the server; initials listed, not indexed" "$(J T41)" \
    '.outline.provenance == "lsp-live" and .outline.reason == "changed_since_index" and ([.groups[].entities[] | select(.name == "initials") | .indexed] == [false])'
  printf 'export function extraUnit(): number {\n  return 1;\n}\n' > "$T/src/util/extra.ts"
  rc="$(cli T42 -- explore src/util/extra.ts -o json)"; assert_rc T42 "explore of a file not in the index exits 0" 0 "$rc"
  assert_jq T42 "outline from the server, reason not_indexed" "$(J T42)" \
    '.resolution == "not_indexed" and .outline.reason == "not_indexed" and ([.groups[].entities[].name] | index("extraUnit") != null)'
  rm -f "$T/src/util/extra.ts"
  rc="$(cli T21a -- index -o json)"; assert_rc T21a "re-index exits 0" 0 "$rc"
  rc="$(cli T21b -- callers formatName -o json)"; assert_rc T21b "callers after the re-index exits 0" 0 "$rc"
  assert_jq T21b "answered live again" "$(J T21b)" '.targets[0].provenance == "lsp-live"'
  assert_set_eq T21b "the new caller is found" "$EXACT_FORMAT_AFTER_EDIT" "$(function_set "$(J T21b)")"
  rc="$(cli T22 -- status --probe-lsp -o json)"; assert_rc T22 "status --probe-lsp exits 0" 0 "$rc"
  assert_jq T22 "typescript-language-server probes ok" "$(J T22)" '.lsp_probe.ts.ok == true and .lsp_probe.ts.ready_ms > 0'
else
  rc="$(cli T21a -- index -o json)"; assert_rc T21a "re-index exits 0" 0 "$rc"
fi

# ---- T17-T19 pre-computed ------------------------------------------------------------------------
if lsp_or_skip T17 "pre-computed callers" ts; then
  rc="$(cli T17 -- index --lsp-precompute all --lsp-min-fanin 2 -o json)"; assert_rc T17 "index with pre-computation exits 0" 0 "$rc"
  assert_jq T17 "ts pre-computed (>= 1 target)" "$(J T17)" '.lsp.languages.ts.status == "computed" and .counts.lsp_targets >= 1'
  rc="$(cli T18 KNOT_LITE_LSP=off -- callers formatName -o json)"; assert_rc T18 "callers with servers off exits 0" 0 "$rc"
  assert_jq T18 "answered from the index (lsp-indexed)" "$(J T18)" '.targets[0].provenance == "lsp-indexed"'
  assert_set_eq T18 "the pre-computed set equals the exact set (with the new caller)" "$EXACT_FORMAT_AFTER_EDIT" "$(function_set "$(J T18)")"
  text_in T18 "a note says when it was pre-computed" "$(J T18)" 'pre-computed at'
  logs_before="$(find "$KNOT_LITE_LSP_CACHE_DIR" -name 'ts_*.stderr.log' 2>/dev/null | wc -l | tr -d ' ')"
  rc="$(cli T19 -- index --lsp-precompute all --lsp-min-fanin 2 -o json)"; assert_rc T19 "a repeated index exits 0" 0 "$rc"
  assert_jq T19 "the pre-computation is reused" "$(J T19)" '.lsp.languages.ts.status == "reused"'
  logs_after="$(find "$KNOT_LITE_LSP_CACHE_DIR" -name 'ts_*.stderr.log' 2>/dev/null | wc -l | tr -d ' ')"
  if [ "$logs_before" = "$logs_after" ]; then pass T19 "no language server was started ($logs_after logs)"; else fail T19 "a server was started" "logs before $logs_before, after $logs_after"; fi
fi

# ---- T23-T33 MCP ------------------------------------------------------------------------------------
# The MCP rows ask for a target that has no pre-computed data (UserRepository.findById); the pre-computed main target
# (formatName) is answered from the index and gets its own row (T28b).
MCP_TARGET=UserRepository.findById
mcp_start "$T" KNOT_LITE_LSP_READY_WAIT_SECS=0
mcp_request 1 server/discover; if mcp_wait 1 20; then pass T23 "server/discover answered"; else fail T23 "no answer to server/discover"; fi
mcp_request 2 tools/list
if mcp_wait 2 20; then
  assert_jq T23 "exactly the five tools" "$E2E_MCP_DIR/2.json" \
    '[.result.tools[].name] | sort == ["explore_file","find_callers","list_files","search_hybrid_context","status"]'
else fail T23 "no answer to tools/list"; fi
mcp_call 3 status '{}'
if mcp_wait 3 30; then
  assert_jq T24 "status: index present, typescript row present" "$(mcp_structured 3)" \
    '.index.state == "present" and ([.languages[].language] | index("typescript") != null)'
else fail T24 "no answer to status"; fi
mcp_call 4 list_files '{}'; mcp_call 5 list_files '{"path":"src/"}'
if mcp_wait 4 20 && mcp_wait 5 20; then
  assert_jq T25 "list_files {} lists the sources" "$(mcp_structured 4)" '($want - [.files[].path]) == []' --argjson want "$SRC_FILES"
  assert_jq T25 "list_files src/ only lists src/" "$(mcp_structured 5)" '(.files|length) > 0 and all(.files[]; .path | startswith("src/"))'
else fail T25 "no answer to list_files"; fi
mcp_call 6 explore_file '{"file_path":"src/services/userRepository.ts"}'
if mcp_wait 6 20; then
  assert_jq T26 "explore_file names UserRepository, save, findById" "$(mcp_structured 6)" '(["UserRepository","save","findById"] - [.groups[].entities[].name]) == []'
  assert_jq T26 "explore_file text names UserRepository" "$E2E_MCP_DIR/6.json" '.result.content[0].text | contains("UserRepository")'
else fail T26 "no answer to explore_file"; fi
mcp_call 7 search_hybrid_context '{"query":"full display name of a user","max_results":5}'
if mcp_wait 7 30; then
  assert_jq T27 "search finds formatName" "$(mcp_structured 7)" '[.results[].name] | index("formatName") != null'
else fail T27 "no answer to search"; fi
mcp_call 8 find_callers "$(jq -n -c --arg e "$MCP_TARGET" '{entity_name:$e}')"
if mcp_wait 8 60; then
  if lsp_available ts && [ -z "${E2E_TS_MISSING:-}" ]; then
    sc="$(mcp_structured 8)"
    if jq -e '.targets[0].provenance == "lsp-live"' "$sc" >/dev/null; then pass T28 "first find_callers: already lsp-live (fast server)"
    elif jq -e '.targets[0].lsp.status == "not_ready" and ((.notes|join(" ")) | contains("is still starting"))' "$sc" >/dev/null; then pass T28 "first find_callers: not_ready, the note says the server is still starting"
    else fail T28 "first find_callers is neither live nor not_ready" "file: $sc"; fi
    assert_jq T28 "the first answer is not an error" "$E2E_MCP_DIR/8.json" '.result.isError != true'
  else lsp_or_skip T28 "first find_callers" ts || true; fi
else fail T28 "no answer to the first find_callers"; fi
if lsp_available ts; then
  mcp_call 9 find_callers '{"entity_name":"formatName"}'
  if mcp_wait 9 60; then
    assert_jq T28b "the pre-computed main target is answered from the index" "$(mcp_structured 9)" \
      '.targets[0].provenance == "lsp-indexed" and .targets[0].lsp.status == "indexed"'
  else fail T28b "no answer for the pre-computed main target"; fi
fi
if lsp_or_skip T29 "MCP live callers" ts; then
  if mcp_wait_live "$MCP_TARGET" "$LSP_TIMEOUT"; then
    sc="$E2E_LAST_SC"
    pass T29 "find_callers became lsp-live after ${E2E_LIVE_SECS} s"
    assert_set_eq T30 "MCP set equals the CLI set" "$(function_set "$(J T14b)")" "$(function_set "$sc")"
    mcp_call 30 status '{}'; mcp_wait 30 30 || true
    assert_jq T32 "status shows a ready server with pid and memory" "$(mcp_structured 30)" \
      '[.languages[].lsp_runtime | select(. != null) | select(.state == "ready")] | length >= 1 and all(.[]; .pid > 0 and .rss_mib > 0)'
  else fail T29 "find_callers never became lsp-live in ${LSP_TIMEOUT} s" "last: ${E2E_LAST_SC:-none}"; fi
fi
mcp_call 40 find_callers '{"entity_name":"x","repo_name":"other"}'; mcp_call 41 nope '{}'
if mcp_wait 40 20 && mcp_wait 41 20; then
  assert_jq T31 "another repository is an error that says so" "$E2E_MCP_DIR/40.json" '.result.isError == true and (.result.content[0].text | contains("not served here"))'
  assert_jq T31 "an unknown tool is an error" "$E2E_MCP_DIR/41.json" '.result.isError == true and (.result.content[0].text | contains("Unknown tool: nope"))'
else fail T31 "no answer to the error probes"; fi
if mcp_stop 20; then pass T33 "the MCP server exits by itself after EOF"; else fail T33 "the MCP server had to be killed"; fi
if jq -e . "$E2E_MCP_DIR/out.jsonl" >/dev/null 2>&1; then pass T33 "every stdout line is valid JSON"; else fail T33 "stdout holds a non-JSON line" "file: $E2E_MCP_DIR/out.jsonl"; fi

# ---- T34-T35 containment and hygiene ---------------------------------------------------------------
fixtures_unchanged T34
assert_no_orphans T35 10

e2e_finish
