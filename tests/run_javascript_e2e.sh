#!/usr/bin/env bash
# E2E: JavaScript (typescript-language-server over jsconfig.json). Usage: ./tests/run_javascript_e2e.sh
# Rows follow Appendix B of the Phase 4 plan; the expected answers are those of Appendix A.3.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/e2e/lib.sh"
e2e_init javascript
e2e_baseline
J_REPO="$(prepare_work javascript)"
use_repo "$J_REPO"
LSP_TIMEOUT="${KNOT_LITE_E2E_LSP_TIMEOUT:-300}"
# The server needs the `typescript` package next to the project; a symlink to the global one, no network.
if link_typescript "$J_REPO"; then :; else E2E_TS_MISSING=1; fi

SRC_FILES='["src/pricing.js","src/cart.js","src/checkout.js","src/legacy.cjs"]'
EXACT_DISCOUNT='["src/cart.js#total","src/checkout.js#quote"]'
DISCOUNT_FILE_ROWS='["src/cart.js","src/checkout.js"]'
EXACT_ADD='["src/checkout.js#checkout"]'
EXACT_LEGACY='["src/legacy.cjs#legacyReport"]'
LEGACY_FILE_ROWS='["src/legacy.cjs"]'
EXACT_DISCOUNT_AFTER_EDIT='["src/cart.js#total","src/checkout.js#quote","src/pricing.js#unitPrice"]'

# ---- J01 versions ---------------------------------------------------------------------------
rc="$(run_cli J01a -- --version)"; assert_rc J01a "knot-lite --version exits 0" 0 "$rc"
text_in J01a "knot-lite --version prints $E2E_VERSION" "$(J J01a)" "$E2E_VERSION_RE"
"$KLM" --version > "$E2E_RUN/cli/J01b.json" 2>&1 || true
text_in J01b "knot-lite-mcp --version prints $E2E_VERSION" "$(J J01b)" "$E2E_VERSION_RE"

# ---- J02-J03 before the index -------------------------------------------------------------------
rc="$(cli J02 -- status -o json)"; assert_rc J02 "status before the index exits 0" 0 "$rc"
assert_jq J02 "index state is absent" "$(J J02)" '.index.state == "absent"'
if lsp_available ts && [ -z "${E2E_TS_MISSING:-}" ]; then want_tier=3; else want_tier=2; fi
assert_jq J02 "javascript language row has tier $want_tier" "$(J J02)" \
  '[.languages[] | select(.language == "javascript") | .tier] == [$t]' --argjson t "$want_tier"
rc="$(cli J03 -- callers discountedPrice -o json)"; assert_rc J03 "callers without an index exits 2" 2 "$rc"
text_in J03 "the diagnostic says there is no index" "$(E J03)" 'has no index'

# ---- J04-J05 index ------------------------------------------------------------------------------
rc="$(cli J04 -- index --lsp-precompute none -o json)"; assert_rc J04 "index exits 0" 0 "$rc"
assert_jq J04 "entities and located columns counted" "$(J J04)" '.counts.entities > 0 and .counts.columns_located > 0'
if [ -f "$J_REPO/.knot-lite/manifest.toml" ] && [ -f "$J_REPO/.knot-lite/.gitignore" ]; then pass J04 "manifest and .gitignore written"; else fail J04 "manifest or .gitignore missing" "in $J_REPO/.knot-lite"; fi
text_in J04 "manifest pins knot 1.11.3" "$J_REPO/.knot-lite/manifest.toml" '^knot_version = "1\.11\.3"'
rc="$(cli J05 -- status -o json)"; assert_rc J05 "status after the index exits 0" 0 "$rc"
assert_jq J05 "index present and fresh" "$(J J05)" '.index.state == "present" and .index.details.freshness.state == "fresh"'

# ---- J06-J09 files ------------------------------------------------------------------------------
rc="$(cli J06 -- files -o json)"; assert_rc J06 "files exits 0" 0 "$rc"
assert_jq J06 "every source file is listed" "$(J J06)" '($want - [.files[].path]) == []' --argjson want "$SRC_FILES"
rc="$(cli J07 -- files -p src/ -o json)"; assert_rc J07 "files -p src/ exits 0" 0 "$rc"
assert_jq J07 "non-empty and every path under src/" "$(J J07)" '(.files|length) > 0 and all(.files[]; .path | startswith("src/"))'
rc="$(cli J08 -- files -p 'src/*' -o json)"; assert_rc J08 "files with a glob exits 0" 0 "$rc"
assert_jq J08 "glob mode, non-empty, every path matches src/*" "$(J J08)" '.path_mode == "glob" and (.files|length) > 0 and all(.files[]; .path | test("^src/[^/]+$"))'
rc="$(cli J09 -- files -r other -o json)"; assert_rc J09 "files -r other exits 1" 1 "$rc"

# ---- J10-J12 explore and search -------------------------------------------------------------------
rc="$(cli J10 -- explore src/cart.js -o json)"; assert_rc J10 "explore (json) exits 0" 0 "$rc"
assert_jq J10 "entities include Cart, add and total" "$(J J10)" '(["Cart","add","total"] - [.groups[].entities[].name]) == []'
rc="$(cli J10m -- explore src/cart.js -o markdown)"; assert_rc J10m "explore (markdown) exits 0" 0 "$rc"
text_in J10m "the markdown names Cart" "$(J J10m)" 'Cart'
rc="$(cli J11a -- search "price after the discount rate" -o json)"; assert_rc J11a "search 1 exits 0" 0 "$rc"
assert_jq J11a "discountedPrice is in the top 5" "$(J J11a)" '[.results[:5][].name] | index("discountedPrice") != null'
rc="$(cli J11b -- search "checkout a list of items with a member discount" -o json)"; assert_rc J11b "search 2 exits 0" 0 "$rc"
assert_jq J11b "checkout is in the top 5" "$(J J11b)" '[.results[:5][].name] | index("checkout") != null'
rc="$(cli J12a -- search "price after the discount rate" -k callable -o json)"; assert_rc J12a "search -k callable exits 0" 0 "$rc"
assert_jq J12a "every hit is a function or a method" "$(J J12a)" '(.results|length) > 0 and all(.results[]; .kind | test("function|method"))'
rc="$(cli J12b -- search "price after the discount rate" -p src/ -o json)"; assert_rc J12b "search -p exits 0" 0 "$rc"
assert_jq J12b "every hit is under src/" "$(J J12b)" '(.results|length) > 0 and all(.results[]; .file | startswith("src/"))'

# ---- J36-J39 Phase 5: globs, target tiers, search modes, explore with servers off -----------------
rc="$(cli J36a -- files -p 'src/*' -o json)"; assert_rc J36a "files with src/* exits 0" 0 "$rc"
assert_jq J36a "exactly the fixture's sources, glob mode" "$(J J36a)" '.path_mode == "glob" and ([.files[].path] | sort) == ($want | sort)' --argjson want "$SRC_FILES"
rc="$(cli J36b -- search "price after the discount rate" -p 'src/p*.js' -m 10 -o json)"; assert_rc J36b "search with a glob exits 0" 0 "$rc"
assert_jq J36b "glob mode, every hit matches src/p*.js" "$(J J36b)" '.filters.path_mode == "glob" and (.results|length) > 0 and all(.results[]; .file | test("^src/p[^/]*\\.js$"))'
rc="$(cli J37a -- callers iscountedPri --no-lsp -o json)"; assert_rc J37a "callers with a substring exits 0" 0 "$rc"
assert_jq J37a "fuzzy tier, discountedPrice is target #1" "$(J J37a)" '.resolution.tier == "fuzzy" and .targets[0].name == "discountedPrice"'
rc="$(cli J37b -- callers qu --no-lsp -o json)"; assert_rc J37b "callers with a short prefix exits 0" 0 "$rc"
assert_jq J37b "name_prefix tier finds quote" "$(J J37b)" '.resolution.tier == "name_prefix" and ([.targets[].name] | index("quote") != null)'
rc="$(cli J37d -- callers discountedPrice --no-lsp -o json)"; assert_rc J37d "callers with an exact name exits 0" 0 "$rc"
assert_jq J37d "the exact name still wins over fuzzy" "$(J J37d)" '.resolution.tier == "exact_name" and .targets[0].name == "discountedPrice"'
rc="$(cli J38a KNOT_LITE_SEARCH=hybrid -- search "price after the discount rate" -o json)"; assert_rc J38a "hybrid search exits 0" 0 "$rc"
assert_jq J38a "search/v2, hybrid, every hit has a cosine and channels" "$(J J38a)" '.schema == "knot-lite/search/v2" and .provenance == "hybrid" and all(.results[]; (.cosine | type) == "number" and (.channels | length) > 0)'
rc="$(cli J38b KNOT_LITE_SEARCH=hybrid -- search "discounted price" -o json)"; assert_rc J38b "hybrid identifier search exits 0" 0 "$rc"
assert_jq J38b "discountedPrice is rank 1" "$(J J38b)" '.results[0].name == "discountedPrice"'
rc="$(cli J38c -- search "price after the discount rate" -o json)"; assert_rc J38c "default search exits 0" 0 "$rc"
assert_jq J38c "vector by default: score equals cosine" "$(J J38c)" '.provenance == "vector" and all(.results[]; .score == .cosine)'
rc="$(cli J39 KNOT_LITE_LSP=off -- explore src/pricing.js -o json)"; assert_rc J39 "explore with servers off exits 0" 0 "$rc"
assert_jq J39 "lsp off, every count heuristic" "$(J J39)" '.schema == "knot-lite/explore/v2" and .lsp.status == "off" and all(.groups[].entities[]; .refs_provenance == "heuristic")'

# ---- J13 heuristic ------------------------------------------------------------------------------
rc="$(cli J13 -- callers discountedPrice --no-lsp -o json)"; assert_rc J13 "callers --no-lsp exits 0" 0 "$rc"
assert_jq J13 "provenance is heuristic and the notes say so" "$(J J13)" \
  '.targets[0].provenance == "heuristic" and (.notes|length) > 0 and .targets[0].lsp.status == "disabled"'
assert_subset J13 "the heuristic callers contain the direct callers" "$EXACT_DISCOUNT" "$(function_set "$(J J13)")"

# ---- J14-J15 live ---------------------------------------------------------------------------------
if lsp_or_skip J14 "live callers" ts; then
  live_exact J14a discountedPrice "$EXACT_DISCOUNT" "$DISCOUNT_FILE_ROWS"
  live_exact J14b Cart.add "$EXACT_ADD"
  live_exact J14c legacyTotal "$EXACT_LEGACY" "$LEGACY_FILE_ROWS"
  rc="$(cli J15 -- callers discountedPrice -o table)"; assert_rc J15 "callers (table) exits 0" 0 "$rc"
  text_in J15 "the table marks the answer [lsp-live]" "$(J J15)" '\[lsp-live\]'
  rc="$(cli J40 -- explore src/pricing.js -o json)"; assert_rc J40 "explore with a live server exits 0" 0 "$rc"
  assert_jq J40 "lsp ok; discountedPrice counted live (4 caller containers)" "$(J J40)" \
    '.lsp.status == "ok" and ([.groups[].entities[] | select(.name == "discountedPrice") | [.refs_provenance, .referenced_by]] == [["lsp-live", 4]])'
fi

# ---- J16 a server that does not resolve ----------------------------------------------------------
rc="$(cli J16 KNOT_LITE_LSP_TS=/nonexistent/server -- callers discountedPrice -o json)"; assert_rc J16 "an unresolvable server still exits 0" 0 "$rc"
assert_jq J16 "degrades to heuristic with status no_server" "$(J J16)" '.targets[0].provenance == "heuristic" and .targets[0].lsp.status == "no_server"'
text_in J16 "the diagnostic says it does not resolve to an executable" "$(J J16)" 'does not resolve to an executable'

# A pre-computed target keeps its pre-computed answer after an edit (lsp-indexed outranks the stale-file check,
# P3-D10), so the stale rows run first, while no target has pre-computed data; the pre-computation then runs
# on the edited fixture.
# ---- J20-J22 stale target, re-index --------------------------------------------------------------
cat >> "$J_REPO/src/pricing.js" <<'JS'

/** Price of a single item without discount. */
export function unitPrice(price) {
  return discountedPrice(price, 1, 0);
}
JS
if lsp_or_skip J20 "stale target" ts; then
  rc="$(cli J20 -- callers discountedPrice -o json)"; assert_rc J20 "callers after an edit exits 0" 0 "$rc"
  assert_jq J20 "the edited file's target is reanchored live with new caller having indexed false" "$(J J20)" \
    '.targets[0].lsp.status == "ok" and .targets[0].provenance == "lsp-live" and .targets[0].lsp.reanchored == true and ([.targets[0].callers.CALLS[] | select(.name == "unitPrice") | .indexed] == [false])'
  text_in J20 "a note says target was re-anchored" "$(J J20)" 're-anchored'
  cp "$J_REPO/src/pricing.js" "$J_REPO/src/pricing.js.bak"
  grep -v 'export function discountedPrice' "$J_REPO/src/pricing.js.bak" > "$J_REPO/src/pricing.js"
  rc="$(cli J44 -- callers discountedPrice -o json)"; assert_rc J44 "callers with deleted declaration exits 0" 0 "$rc"
  assert_jq J44 "target declaration gone is stale_target" "$(J J44)" '.targets[0].lsp.status == "stale_target" and .targets[0].provenance == "heuristic"'
  mv "$J_REPO/src/pricing.js.bak" "$J_REPO/src/pricing.js"
  cp "$J_REPO/src/pricing.js" "$J_REPO/src/pricing.js.bak"
  printf '// shifted\n// shifted\n' | cat - "$J_REPO/src/pricing.js.bak" > "$J_REPO/src/pricing.js"
  rc="$(cli J45 -- callers discountedPrice -o json)"; assert_rc J45 "callers with shifted lines exits 0" 0 "$rc"
  assert_jq J45 "reanchored live with callers matching" "$(J J45)" '.targets[0].lsp.status == "ok" and .targets[0].lsp.reanchored == true'
  assert_set_eq J45 "caller set matches pre-shift answer" "$(function_set "$(J J20)")" "$(function_set "$(J J45)")"
  mv "$J_REPO/src/pricing.js.bak" "$J_REPO/src/pricing.js"
  rc="$(cli J41 -- explore src/pricing.js -o json)"; assert_rc J41 "explore of the edited file exits 0" 0 "$rc"
  assert_jq J41 "outline from the server; unitPrice listed, not indexed" "$(J J41)" \
    '.outline.provenance == "lsp-live" and .outline.reason == "changed_since_index" and ([.groups[].entities[] | select(.name == "unitPrice") | .indexed] == [false])'
  printf 'export function extraUnit() {\n  return 1;\n}\n' > "$J_REPO/src/extra.js"
  rc="$(cli J42 -- explore src/extra.js -o json)"; assert_rc J42 "explore of a file not in the index exits 0" 0 "$rc"
  assert_jq J42 "outline from the server, reason not_indexed" "$(J J42)" \
    '.resolution == "not_indexed" and .outline.reason == "not_indexed" and ([.groups[].entities[].name] | index("extraUnit") != null)'
  rm -f "$J_REPO/src/extra.js"
  rc="$(cli J21a -- index -o json)"; assert_rc J21a "re-index exits 0" 0 "$rc"
  rc="$(cli J21b -- callers discountedPrice -o json)"; assert_rc J21b "callers after the re-index exits 0" 0 "$rc"
  assert_jq J21b "answered live again" "$(J J21b)" '.targets[0].provenance == "lsp-live"'
  assert_set_eq J21b "the new caller is found" "$EXACT_DISCOUNT_AFTER_EDIT" "$(function_set "$(J J21b)")"
  rc="$(cli J22 -- status --probe-lsp -o json)"; assert_rc J22 "status --probe-lsp exits 0" 0 "$rc"
  assert_jq J22 "typescript-language-server probes ok" "$(J J22)" '.lsp_probe.ts.ok == true and .lsp_probe.ts.ready_ms > 0'
else
  rc="$(cli J21a -- index -o json)"; assert_rc J21a "re-index exits 0" 0 "$rc"
fi

# ---- J17-J19 pre-computed ------------------------------------------------------------------------
if lsp_or_skip J17 "pre-computed callers" ts; then
  rc="$(cli J17 -- index --lsp-precompute all --lsp-min-fanin 2 -o json)"; assert_rc J17 "index with pre-computation exits 0" 0 "$rc"
  assert_jq J17 "ts pre-computed (>= 1 target)" "$(J J17)" '.lsp.languages.ts.status == "computed" and .counts.lsp_targets >= 1'
  rc="$(cli J18 KNOT_LITE_LSP=off -- callers discountedPrice -o json)"; assert_rc J18 "callers with servers off exits 0" 0 "$rc"
  assert_jq J18 "answered from the index (lsp-indexed)" "$(J J18)" '.targets[0].provenance == "lsp-indexed"'
  assert_set_eq J18 "the pre-computed set equals the exact set (with the new caller)" "$EXACT_DISCOUNT_AFTER_EDIT" "$(function_set "$(J J18)")"
  text_in J18 "a note says when it was pre-computed" "$(J J18)" 'pre-computed at'
  logs_before="$(find "$KNOT_LITE_LSP_CACHE_DIR" -name 'ts_*.stderr.log' 2>/dev/null | wc -l | tr -d ' ')"
  rc="$(cli J19 -- index --lsp-precompute all --lsp-min-fanin 2 -o json)"; assert_rc J19 "a repeated index exits 0" 0 "$rc"
  assert_jq J19 "the pre-computation is reused" "$(J J19)" '.lsp.languages.ts.status == "reused"'
  logs_after="$(find "$KNOT_LITE_LSP_CACHE_DIR" -name 'ts_*.stderr.log' 2>/dev/null | wc -l | tr -d ' ')"
  if [ "$logs_before" = "$logs_after" ]; then pass J19 "no language server was started ($logs_after logs)"; else fail J19 "a server was started" "logs before $logs_before, after $logs_after"; fi
fi

# ---- J23-J33 MCP ------------------------------------------------------------------------------------
# The MCP rows ask for a target that has no pre-computed data (Cart.add); the pre-computed main target
# (discountedPrice) is answered from the index and gets its own row (J28b).
MCP_TARGET=Cart.add
mcp_start "$J_REPO" KNOT_LITE_LSP_READY_WAIT_SECS=0
mcp_request 1 server/discover; if mcp_wait 1 20; then pass J23 "server/discover answered"; else fail J23 "no answer to server/discover"; fi
mcp_request 2 tools/list
if mcp_wait 2 20; then
  assert_jq J23 "exactly the five tools" "$E2E_MCP_DIR/2.json" \
    '[.result.tools[].name] | sort == ["explore_file","find_callers","list_files","search_hybrid_context","status"]'
else fail J23 "no answer to tools/list"; fi
mcp_call 3 status '{}'
if mcp_wait 3 30; then
  assert_jq J24 "status: index present, javascript row present" "$(mcp_structured 3)" \
    '.index.state == "present" and ([.languages[].language] | index("javascript") != null)'
else fail J24 "no answer to status"; fi
mcp_call 4 list_files '{}'; mcp_call 5 list_files '{"path":"src/"}'
if mcp_wait 4 20 && mcp_wait 5 20; then
  assert_jq J25 "list_files {} lists the sources" "$(mcp_structured 4)" '($want - [.files[].path]) == []' --argjson want "$SRC_FILES"
  assert_jq J25 "list_files src/ only lists src/" "$(mcp_structured 5)" '(.files|length) > 0 and all(.files[]; .path | startswith("src/"))'
else fail J25 "no answer to list_files"; fi
mcp_call 6 explore_file '{"file_path":"src/cart.js"}'
if mcp_wait 6 20; then
  assert_jq J26 "explore_file names Cart, add, total" "$(mcp_structured 6)" '(["Cart","add","total"] - [.groups[].entities[].name]) == []'
  assert_jq J26 "explore_file text names Cart" "$E2E_MCP_DIR/6.json" '.result.content[0].text | contains("Cart")'
else fail J26 "no answer to explore_file"; fi
mcp_call 7 search_hybrid_context '{"query":"price after the discount rate","max_results":5}'
if mcp_wait 7 30; then
  assert_jq J27 "search finds discountedPrice" "$(mcp_structured 7)" '[.results[].name] | index("discountedPrice") != null'
else fail J27 "no answer to search"; fi
mcp_call 8 find_callers "$(jq -n -c --arg e "$MCP_TARGET" '{entity_name:$e}')"
if mcp_wait 8 60; then
  if lsp_available ts && [ -z "${E2E_TS_MISSING:-}" ]; then
    sc="$(mcp_structured 8)"
    if jq -e '.targets[0].provenance == "lsp-live"' "$sc" >/dev/null; then pass J28 "first find_callers: already lsp-live (fast server)"
    elif jq -e '.targets[0].lsp.status == "not_ready" and ((.notes|join(" ")) | contains("is still starting"))' "$sc" >/dev/null; then pass J28 "first find_callers: not_ready, the note says the server is still starting"
    else fail J28 "first find_callers is neither live nor not_ready" "file: $sc"; fi
    assert_jq J28 "the first answer is not an error" "$E2E_MCP_DIR/8.json" '.result.isError != true'
  else lsp_or_skip J28 "first find_callers" ts || true; fi
else fail J28 "no answer to the first find_callers"; fi
if lsp_available ts; then
  mcp_call 9 find_callers '{"entity_name":"discountedPrice"}'
  if mcp_wait 9 60; then
    assert_jq J28b "the pre-computed main target is answered from the index" "$(mcp_structured 9)" \
      '.targets[0].provenance == "lsp-indexed" and .targets[0].lsp.status == "indexed"'
  else fail J28b "no answer for the pre-computed main target"; fi
fi
if lsp_or_skip J29 "MCP live callers" ts; then
  if mcp_wait_live "$MCP_TARGET" "$LSP_TIMEOUT"; then
    sc="$E2E_LAST_SC"
    pass J29 "find_callers became lsp-live after ${E2E_LIVE_SECS} s"
    assert_set_eq J30 "MCP set equals the CLI set" "$(function_set "$(J J14b)")" "$(function_set "$sc")"
    mcp_call 30 status '{}'; mcp_wait 30 30 || true
    assert_jq J32 "status shows a ready server with pid and memory" "$(mcp_structured 30)" \
      '[.languages[].lsp_runtime | select(. != null) | select(.state == "ready")] | length >= 1 and all(.[]; .pid > 0 and .rss_mib > 0)'
  else fail J29 "find_callers never became lsp-live in ${LSP_TIMEOUT} s" "last: ${E2E_LAST_SC:-none}"; fi
fi
mcp_call 40 find_callers '{"entity_name":"x","repo_name":"other"}'; mcp_call 41 nope '{}'
if mcp_wait 40 20 && mcp_wait 41 20; then
  assert_jq J31 "another repository is an error that says so" "$E2E_MCP_DIR/40.json" '.result.isError == true and (.result.content[0].text | contains("not served here"))'
  assert_jq J31 "an unknown tool is an error" "$E2E_MCP_DIR/41.json" '.result.isError == true and (.result.content[0].text | contains("Unknown tool: nope"))'
else fail J31 "no answer to the error probes"; fi
if mcp_stop 20; then pass J33 "the MCP server exits by itself after EOF"; else fail J33 "the MCP server had to be killed"; fi
if jq -e . "$E2E_MCP_DIR/out.jsonl" >/dev/null 2>&1; then pass J33 "every stdout line is valid JSON"; else fail J33 "stdout holds a non-JSON line" "file: $E2E_MCP_DIR/out.jsonl"; fi

# ---- J34-J35 containment and hygiene ---------------------------------------------------------------
fixtures_unchanged J34
assert_no_orphans J35 10

e2e_finish
