#!/usr/bin/env bash
# E2E: Rust (rust-analyzer). Usage: ./tests/run_rust_e2e.sh
# Rows follow Appendix B of the Phase 4 plan; the expected answers are those of Appendix A.1.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/e2e/lib.sh"
e2e_init rust
e2e_baseline
R="$(prepare_work rust)"
LSP_TIMEOUT="${KNOT_LITE_E2E_LSP_TIMEOUT:-300}"

SRC_FILES='["src/lib.rs","src/shapes.rs","src/geometry.rs","src/report.rs"]'
# Exact language-server answers (function level).
EXACT_TOTAL='["src/geometry.rs#pair_area","src/report.rs#scene_area"]'
EXACT_PAIR='["src/report.rs#summary"]'
EXACT_NEW='["src/geometry.rs#pair_area","src/report.rs#scene_area"]'
EXACT_TOTAL_AFTER_EDIT='["src/geometry.rs#pair_area","src/geometry.rs#triple_circle","src/report.rs#scene_area"]'

use_repo "$R"

# ---- R01 versions ---------------------------------------------------------------------------
rc="$(run_cli R01a -- --version)"; assert_rc R01a "knot-lite --version exits 0" 0 "$rc"
text_in R01a "knot-lite --version prints 0.4.0" "$(J R01a)" '0\.4\.0'
"$KLM" --version > "$E2E_RUN/cli/R01b.json" 2>&1 || true
text_in R01b "knot-lite-mcp --version prints 0.4.0" "$(J R01b)" '0\.4\.0'

# ---- R02-R03 before the index -------------------------------------------------------------------
rc="$(cli R02 -- status -o json)"; assert_rc R02 "status before the index exits 0" 0 "$rc"
assert_jq R02 "index state is absent" "$(J R02)" '.index.state == "absent"'
if lsp_available rust; then want_tier=3; else want_tier=2; fi
assert_jq R02 "rust language row has tier $want_tier" "$(J R02)" \
  '[.languages[] | select(.language == "rust") | .tier] == [$t]' --argjson t "$want_tier"
rc="$(cli R03 -- callers total_area -o json)"; assert_rc R03 "callers without an index exits 2" 2 "$rc"
text_in R03 "the diagnostic says there is no index" "$(E R03)" 'has no index'

# ---- R04-R05 index ------------------------------------------------------------------------------
rc="$(cli R04 -- index --lsp-precompute none -o json)"; assert_rc R04 "index exits 0" 0 "$rc"
assert_jq R04 "entities and located columns counted" "$(J R04)" '.counts.entities > 0 and .counts.columns_located > 0'
if [ -f "$R/.knot-lite/manifest.toml" ] && [ -f "$R/.knot-lite/.gitignore" ]; then pass R04 "manifest and .gitignore written"; else fail R04 "manifest or .gitignore missing" "in $R/.knot-lite"; fi
text_in R04 "manifest pins knot 1.11.3" "$R/.knot-lite/manifest.toml" '^knot_version = "1\.11\.3"'
rc="$(cli R05 -- status -o json)"; assert_rc R05 "status after the index exits 0" 0 "$rc"
assert_jq R05 "index present and fresh" "$(J R05)" '.index.state == "present" and .index.details.freshness.state == "fresh"'

# ---- R06-R09 files ------------------------------------------------------------------------------
rc="$(cli R06 -- files -o json)"; assert_rc R06 "files exits 0" 0 "$rc"
assert_jq R06 "every source file is listed" "$(J R06)" '($want - [.files[].path]) == []' --argjson want "$SRC_FILES"
rc="$(cli R07 -- files -p src/ -o json)"; assert_rc R07 "files -p src/ exits 0" 0 "$rc"
assert_jq R07 "non-empty and every path under src/" "$(J R07)" '(.files|length) > 0 and all(.files[]; .path | startswith("src/"))'
rc="$(cli R08 -- files -p 'src/*' -o json)"; assert_rc R08 "a glob prefix is rejected with rc 1" 1 "$rc"
text_in R08 "the message names the limitation" "$(E R08)" 'glob patterns are not supported'
rc="$(cli R09 -- files -r other -o json)"; assert_rc R09 "files -r other exits 1" 1 "$rc"

# ---- R10-R12 explore and search -------------------------------------------------------------------
rc="$(cli R10 -- explore src/shapes.rs -o json)"; assert_rc R10 "explore (json) exits 0" 0 "$rc"
assert_jq R10 "entities include Shape, Circle, Square and new" "$(J R10)" \
  '(["Shape","Circle","Square","new"] - [.groups[].entities[].name]) == []'
rc="$(cli R10m -- explore src/shapes.rs -o markdown)"; assert_rc R10m "explore (markdown) exits 0" 0 "$rc"
text_in R10m "the markdown names Circle" "$(J R10m)" 'Circle'
rc="$(cli R11a -- search "sum of the areas of every shape" -o json)"; assert_rc R11a "search 1 exits 0" 0 "$rc"
assert_jq R11a "total_area is in the top 5" "$(J R11a)" '[.results[:5][].name] | index("total_area") != null'
rc="$(cli R11b -- search "build a circle from its radius" -o json)"; assert_rc R11b "search 2 exits 0" 0 "$rc"
assert_jq R11b "new or Circle is in the top 5" "$(J R11b)" '[.results[:5][].name] | (index("new") != null or index("Circle") != null)'
rc="$(cli R12a -- search "sum of the areas of every shape" -k callable -o json)"; assert_rc R12a "search -k callable exits 0" 0 "$rc"
assert_jq R12a "every hit is a function or a method" "$(J R12a)" '(.results|length) > 0 and all(.results[]; .kind | test("function|method"))'
rc="$(cli R12b -- search "sum of the areas of every shape" -p src/ -o json)"; assert_rc R12b "search -p exits 0" 0 "$rc"
assert_jq R12b "every hit is under src/" "$(J R12b)" '(.results|length) > 0 and all(.results[]; .file | startswith("src/"))'

# ---- R13 heuristic ------------------------------------------------------------------------------
rc="$(cli R13 -- callers total_area --no-lsp -o json)"; assert_rc R13 "callers --no-lsp exits 0" 0 "$rc"
assert_jq R13 "provenance is heuristic and the notes say so" "$(J R13)" \
  '.targets[0].provenance == "heuristic" and (.notes|length) > 0 and .targets[0].lsp.status == "disabled"'
assert_subset R13 "the heuristic callers contain the direct callers" "$EXACT_TOTAL" "$(function_set "$(J R13)")"

# ---- R14-R15 live ---------------------------------------------------------------------------------
if lsp_or_skip R14 "live callers" rust; then
  live_exact R14a total_area "$EXACT_TOTAL"
  live_exact R14b pair_area "$EXACT_PAIR"
  live_exact R14c new "$EXACT_NEW"
  assert_jq R14c "new: exactly one target; scene_area counted at 2 sites" "$(J R14c)" \
    '(.targets|length) == 1 and ([.targets[0].callers[]?[]? | select(.name == "scene_area") | .sites] | max) == 2'
  rc="$(cli R14d -- callers Circle -o json)"; assert_rc R14d "callers Circle exits 0" 0 "$rc"
  assert_subset R14d "Circle is referenced by pair_area and scene_area" '["src/geometry.rs#pair_area","src/report.rs#scene_area"]' "$(function_set "$(J R14d)")"
  rc="$(cli R15 -- callers total_area -o table)"; assert_rc R15 "callers (table) exits 0" 0 "$rc"
  text_in R15 "the table marks the answer [lsp-live]" "$(J R15)" '\[lsp-live\]'
fi

# ---- R16 a server that does not resolve ----------------------------------------------------------
rc="$(cli R16 KNOT_LITE_LSP_RUST=/nonexistent/server -- callers total_area -o json)"; assert_rc R16 "an unresolvable server still exits 0" 0 "$rc"
assert_jq R16 "degrades to heuristic with status no_server" "$(J R16)" '.targets[0].provenance == "heuristic" and .targets[0].lsp.status == "no_server"'
text_in R16 "the diagnostic says it does not resolve to an executable" "$(J R16)" 'does not resolve to an executable'

# A pre-computed target keeps its pre-computed answer after an edit (lsp-indexed outranks the stale-file check,
# P3-D10), so the stale rows run first, while no target has pre-computed data; the pre-computation then runs
# on the edited fixture.
# ---- R20-R22 stale target, re-index --------------------------------------------------------------
cat >> "$R/src/geometry.rs" <<'RS'

/// Three times the area of one circle.
pub fn triple_circle(size: f64) -> f64 {
    3.0 * total_area(&[&crate::shapes::Circle::new(size)])
}
RS
if lsp_or_skip R20 "stale target" rust; then
  rc="$(cli R20 -- callers total_area -o json)"; assert_rc R20 "callers after an edit exits 0" 0 "$rc"
  assert_jq R20 "the edited file's target is stale and falls back" "$(J R20)" '.targets[0].lsp.status == "stale_target" and .targets[0].provenance == "heuristic"'
  text_in R20 "a note says the file changed since the last index" "$(J R20)" 'changed since the last index'
  rc="$(cli R21a -- index -o json)"; assert_rc R21a "re-index exits 0" 0 "$rc"
  rc="$(cli R21b -- callers total_area -o json)"; assert_rc R21b "callers after the re-index exits 0" 0 "$rc"
  assert_jq R21b "answered live again" "$(J R21b)" '.targets[0].provenance == "lsp-live"'
  assert_set_eq R21b "the new caller is found" "$EXACT_TOTAL_AFTER_EDIT" "$(function_set "$(J R21b)")"
  rc="$(cli R22 -- status --probe-lsp -o json)"; assert_rc R22 "status --probe-lsp exits 0" 0 "$rc"
  assert_jq R22 "rust-analyzer probes ok" "$(J R22)" '.lsp_probe.rust.ok == true and .lsp_probe.rust.ready_ms > 0'
else
  rc="$(cli R21a -- index -o json)"; assert_rc R21a "re-index exits 0" 0 "$rc"
fi

# ---- R17-R19 pre-computed ------------------------------------------------------------------------
if lsp_or_skip R17 "pre-computed callers" rust; then
  rc="$(cli R17 -- index --lsp-precompute all --lsp-min-fanin 2 -o json)"; assert_rc R17 "index with pre-computation exits 0" 0 "$rc"
  assert_jq R17 "rust pre-computed (>= 2 targets)" "$(J R17)" '.lsp.languages.rust.status == "computed" and .counts.lsp_targets >= 2'
  rc="$(cli R18 KNOT_LITE_LSP=off -- callers total_area -o json)"; assert_rc R18 "callers with servers off exits 0" 0 "$rc"
  assert_jq R18 "answered from the index (lsp-indexed)" "$(J R18)" '.targets[0].provenance == "lsp-indexed"'
  assert_set_eq R18 "the pre-computed set equals the exact set (with the new caller)" "$EXACT_TOTAL_AFTER_EDIT" "$(function_set "$(J R18)")"
  text_in R18 "a note says when it was pre-computed" "$(J R18)" 'pre-computed at'
  logs_before="$(find "$KNOT_LITE_LSP_CACHE_DIR" -name 'rust_*.stderr.log' 2>/dev/null | wc -l | tr -d ' ')"
  rc="$(cli R19 -- index --lsp-precompute all --lsp-min-fanin 2 -o json)"; assert_rc R19 "a repeated index exits 0" 0 "$rc"
  assert_jq R19 "the pre-computation is reused" "$(J R19)" '.lsp.languages.rust.status == "reused"'
  logs_after="$(find "$KNOT_LITE_LSP_CACHE_DIR" -name 'rust_*.stderr.log' 2>/dev/null | wc -l | tr -d ' ')"
  if [ "$logs_before" = "$logs_after" ]; then pass R19 "no language server was started ($logs_after logs)"; else fail R19 "a server was started" "logs before $logs_before, after $logs_after"; fi
fi

# ---- R23-R33 MCP ------------------------------------------------------------------------------------
# The MCP rows ask for a target that has no pre-computed data (pair_area); the pre-computed main target
# (total_area) is answered from the index and gets its own row (R28b).
MCP_TARGET=pair_area
mcp_start "$R" KNOT_LITE_LSP_READY_WAIT_SECS=0
mcp_request 1 server/discover; if mcp_wait 1 20; then pass R23 "server/discover answered"; else fail R23 "no answer to server/discover"; fi
mcp_request 2 tools/list
if mcp_wait 2 20; then
  assert_jq R23 "exactly the five tools" "$E2E_MCP_DIR/2.json" \
    '[.result.tools[].name] | sort == ["explore_file","find_callers","list_files","search_hybrid_context","status"]'
else fail R23 "no answer to tools/list"; fi
mcp_call 3 status '{}'
if mcp_wait 3 30; then
  sc="$(mcp_structured 3)"
  assert_jq R24 "status: index present, rust row present" "$sc" '.index.state == "present" and ([.languages[].language] | index("rust") != null)'
else fail R24 "no answer to status"; fi
mcp_call 4 list_files '{}'; mcp_call 5 list_files '{"path":"src/"}'
if mcp_wait 4 20 && mcp_wait 5 20; then
  assert_jq R25 "list_files {} lists the sources" "$(mcp_structured 4)" '($want - [.files[].path]) == []' --argjson want "$SRC_FILES"
  assert_jq R25 "list_files src/ only lists src/" "$(mcp_structured 5)" '(.files|length) > 0 and all(.files[]; .path | startswith("src/"))'
else fail R25 "no answer to list_files"; fi
mcp_call 6 explore_file '{"file_path":"src/shapes.rs"}'
if mcp_wait 6 20; then
  assert_jq R26 "explore_file names Shape, Circle, Square, new" "$(mcp_structured 6)" '(["Shape","Circle","Square","new"] - [.groups[].entities[].name]) == []'
  assert_jq R26 "explore_file text names Circle" "$E2E_MCP_DIR/6.json" '.result.content[0].text | contains("Circle")'
else fail R26 "no answer to explore_file"; fi
mcp_call 7 search_hybrid_context '{"query":"sum of the areas of every shape","max_results":5}'
if mcp_wait 7 30; then
  assert_jq R27 "search finds total_area" "$(mcp_structured 7)" '[.results[].name] | index("total_area") != null'
else fail R27 "no answer to search"; fi
mcp_call 8 find_callers "$(jq -n -c --arg e "$MCP_TARGET" '{entity_name:$e}')"
if mcp_wait 8 60; then
  if lsp_available rust; then
    sc="$(mcp_structured 8)"
    if jq -e '.targets[0].provenance == "lsp-live"' "$sc" >/dev/null; then pass R28 "first find_callers: already lsp-live (fast server)"
    elif jq -e '.targets[0].lsp.status == "not_ready" and ((.notes|join(" ")) | contains("is still starting"))' "$sc" >/dev/null; then pass R28 "first find_callers: not_ready, the note says the server is still starting"
    else fail R28 "first find_callers is neither live nor not_ready" "file: $sc"; fi
    assert_jq R28 "the first answer is not an error" "$E2E_MCP_DIR/8.json" '.result.isError != true'
  else lsp_or_skip R28 "first find_callers" rust || true; fi
else fail R28 "no answer to the first find_callers"; fi
if lsp_available rust; then
  mcp_call 9 find_callers '{"entity_name":"total_area"}'
  if mcp_wait 9 60; then
    assert_jq R28b "the pre-computed main target is answered from the index" "$(mcp_structured 9)" \
      '.targets[0].provenance == "lsp-indexed" and .targets[0].lsp.status == "indexed"'
  else fail R28b "no answer for the pre-computed main target"; fi
fi
if lsp_or_skip R29 "MCP live callers" rust; then
  if mcp_wait_live "$MCP_TARGET" "$LSP_TIMEOUT"; then
    sc="$E2E_LAST_SC"
    pass R29 "find_callers became lsp-live after ${E2E_LIVE_SECS} s"
    assert_set_eq R30 "MCP set equals the CLI set" "$(function_set "$(J R14b)")" "$(function_set "$sc")"
    mcp_call 30 status '{}'; mcp_wait 30 30 || true
    assert_jq R32 "status shows a ready rust-analyzer with pid and memory" "$(mcp_structured 30)" \
      '[.languages[] | select(.language == "rust") | .lsp_runtime] | .[0] | .state == "ready" and .pid > 0 and .rss_mib > 0'
  else fail R29 "find_callers never became lsp-live in ${LSP_TIMEOUT} s" "last: ${E2E_LAST_SC:-none}"; fi
fi
mcp_call 40 find_callers '{"entity_name":"x","repo_name":"other"}'; mcp_call 41 nope '{}'
if mcp_wait 40 20 && mcp_wait 41 20; then
  assert_jq R31 "another repository is an error that says so" "$E2E_MCP_DIR/40.json" '.result.isError == true and (.result.content[0].text | contains("not served here"))'
  assert_jq R31 "an unknown tool is an error" "$E2E_MCP_DIR/41.json" '.result.isError == true and (.result.content[0].text | contains("Unknown tool: nope"))'
else fail R31 "no answer to the error probes"; fi
if mcp_stop 20; then pass R33 "the MCP server exits by itself after EOF"; else fail R33 "the MCP server had to be killed"; fi
if jq -e . "$E2E_MCP_DIR/out.jsonl" >/dev/null 2>&1; then pass R33 "every stdout line is valid JSON"; else fail R33 "stdout holds a non-JSON line" "file: $E2E_MCP_DIR/out.jsonl"; fi

# ---- R34-R35 containment and hygiene ---------------------------------------------------------------
if [ ! -e "$R/target" ]; then pass R34 "no target/ in the repository"; else fail R34 "rust-analyzer wrote target/ into the repository" "$R/target"; fi
fixtures_unchanged R34
assert_no_orphans R35 10

e2e_finish
