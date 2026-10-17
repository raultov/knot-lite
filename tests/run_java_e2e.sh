#!/usr/bin/env bash
# E2E: Java (jdtls). Maven fixture (rows V*) and a Gradle sub-suite (rows G*).
# Usage: ./tests/run_java_e2e.sh
# Rows follow Appendix B of the Phase 4 plan; the expected answers are those of Appendix A.4 / A.5.
# The G* rows are the end-to-end proof that no Gradle daemon outlives the server that caused it.
set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/e2e/lib.sh"
e2e_init java
e2e_baseline
V="$(prepare_work java)"
use_repo "$V"
LSP_TIMEOUT="${KNOT_LITE_E2E_LSP_TIMEOUT:-300}"
# A first Gradle or Maven import is longer than the default 30 s live budget of one call.
export KNOT_LITE_LSP_DEADLINE_SECS="${KNOT_LITE_LSP_DEADLINE_SECS:-120}"

P=src/main/java/com/example/shop
SRC_FILES="[\"$P/model/Item.java\",\"$P/model/Order.java\",\"$P/service/PriceCalculator.java\",\"$P/service/DefaultPriceCalculator.java\",\"$P/service/DiscountPriceCalculator.java\",\"$P/api/CheckoutController.java\"]"
EXACT_TOTAL="[\"$P/api/CheckoutController.java#checkout\",\"$P/service/DiscountPriceCalculator.java#total\",\"$P/service/DefaultPriceCalculator.java#total\"]"
EXACT_GETITEMS="[\"$P/service/DefaultPriceCalculator.java#total\",\"$P/api/CheckoutController.java#count\"]"
EXACT_ADDITEM="[\"$P/api/CheckoutController.java#single\"]"
IFACE_CONTAINS="[\"$P/service/DiscountPriceCalculator.java#DiscountPriceCalculator\",\"$P/api/CheckoutController.java#CheckoutController\"]"
IFACE_FILE_ROWS="[\"$P/service/DefaultPriceCalculator.java\",\"$P/service/DiscountPriceCalculator.java\",\"$P/api/CheckoutController.java\"]"
EXACT_GETITEMS_AFTER_EDIT="[\"$P/service/DefaultPriceCalculator.java#total\",\"$P/api/CheckoutController.java#count\",\"$P/model/Order.java#size\"]"

# ---- V01 versions ---------------------------------------------------------------------------
rc="$(run_cli V01a -- --version)"; assert_rc V01a "knot-lite --version exits 0" 0 "$rc"
text_in V01a "knot-lite --version prints $E2E_VERSION" "$(J V01a)" "$E2E_VERSION_RE"
"$KLM" --version > "$E2E_RUN/cli/V01b.json" 2>&1 || true
text_in V01b "knot-lite-mcp --version prints $E2E_VERSION" "$(J V01b)" "$E2E_VERSION_RE"

# ---- V02-V03 before the index -------------------------------------------------------------------
rc="$(cli V02 -- status -o json)"; assert_rc V02 "status before the index exits 0" 0 "$rc"
assert_jq V02 "index state is absent" "$(J V02)" '.index.state == "absent"'
if lsp_available java; then want_tier=3; else want_tier=2; fi
assert_jq V02 "java language row has tier $want_tier" "$(J V02)" \
  '[.languages[] | select(.language == "java") | .tier] == [$t]' --argjson t "$want_tier"
rc="$(cli V03 -- callers Order.getItems -o json)"; assert_rc V03 "callers without an index exits 2" 2 "$rc"
text_in V03 "the diagnostic says there is no index" "$(E V03)" 'has no index'

# ---- V04-V05 index ------------------------------------------------------------------------------
rc="$(cli V04 -- index --lsp-precompute none -o json)"; assert_rc V04 "index exits 0" 0 "$rc"
assert_jq V04 "entities and located columns counted" "$(J V04)" '.counts.entities > 0 and .counts.columns_located > 0'
if [ -f "$V/.knot-lite/manifest.toml" ] && [ -f "$V/.knot-lite/.gitignore" ]; then pass V04 "manifest and .gitignore written"; else fail V04 "manifest or .gitignore missing" "in $V/.knot-lite"; fi
text_in V04 "manifest pins knot 1.11.3" "$V/.knot-lite/manifest.toml" '^knot_version = "1\.11\.3"'
rc="$(cli V05 -- status -o json)"; assert_rc V05 "status after the index exits 0" 0 "$rc"
assert_jq V05 "index present and fresh" "$(J V05)" '.index.state == "present" and .index.details.freshness.state == "fresh"'

# ---- V06-V09 files ------------------------------------------------------------------------------
rc="$(cli V06 -- files -o json)"; assert_rc V06 "files exits 0" 0 "$rc"
assert_jq V06 "every source file is listed" "$(J V06)" '($want - [.files[].path]) == []' --argjson want "$SRC_FILES"
rc="$(cli V07 -- files -p src/ -o json)"; assert_rc V07 "files -p src/ exits 0" 0 "$rc"
assert_jq V07 "non-empty and every path under src/" "$(J V07)" '(.files|length) > 0 and all(.files[]; .path | startswith("src/"))'
rc="$(cli V08 -- files -p 'src/**/*.java' -o json)"; assert_rc V08 "files with a glob exits 0" 0 "$rc"
assert_jq V08 "glob mode, non-empty, every path matches src/**/*.java" "$(J V08)" '.path_mode == "glob" and (.files|length) > 0 and all(.files[]; .path | test("^src/.+\\.java$"))'
rc="$(cli V09 -- files -r other -o json)"; assert_rc V09 "files -r other exits 1" 1 "$rc"

# ---- V10-V12 explore and search -------------------------------------------------------------------
rc="$(cli V10 -- explore "$P/service/DiscountPriceCalculator.java" -o json)"; assert_rc V10 "explore (json) exits 0" 0 "$rc"
assert_jq V10 "entities include DiscountPriceCalculator and total" "$(J V10)" \
  '(["DiscountPriceCalculator","total"] - [.groups[].entities[].name]) == []'
rc="$(cli V10m -- explore "$P/service/DiscountPriceCalculator.java" -o markdown)"; assert_rc V10m "explore (markdown) exits 0" 0 "$rc"
text_in V10m "the markdown names DiscountPriceCalculator" "$(J V10m)" 'DiscountPriceCalculator'
rc="$(cli V11a -- search "sums unit price times quantity" -o json)"; assert_rc V11a "search 1 exits 0" 0 "$rc"
assert_jq V11a "DefaultPriceCalculator or total is in the top 5" "$(J V11a)" '[.results[:5][].name] | (index("DefaultPriceCalculator") != null or index("total") != null)'
rc="$(cli V11b -- search "entry point of the checkout flow" -o json)"; assert_rc V11b "search 2 exits 0" 0 "$rc"
assert_jq V11b "CheckoutController or checkout is in the top 5" "$(J V11b)" '[.results[:5][].name] | (index("CheckoutController") != null or index("checkout") != null)'
rc="$(cli V12a -- search "sums unit price times quantity" -k callable -o json)"; assert_rc V12a "search -k callable exits 0" 0 "$rc"
assert_jq V12a "every hit is a function or a method" "$(J V12a)" '(.results|length) > 0 and all(.results[]; .kind | test("function|method"))'
rc="$(cli V12b -- search "sums unit price times quantity" -p "$P/service" -o json)"; assert_rc V12b "search -p exits 0" 0 "$rc"
assert_jq V12b "every hit is under the service package" "$(J V12b)" '(.results|length) > 0 and all(.results[]; .file | startswith($p))' --arg p "$P/service"

# ---- V36-V39 Phase 5: globs, target tiers, search modes, explore with servers off -----------------
rc="$(cli V36a -- files -p 'src/**/*.java' -o json)"; assert_rc V36a "files with src/**/*.java exits 0" 0 "$rc"
assert_jq V36a "exactly the fixture's sources, glob mode" "$(J V36a)" '.path_mode == "glob" and ([.files[].path] | sort) == ($want | sort)' --argjson want "$SRC_FILES"
rc="$(cli V36b -- search "sums unit price times quantity" -p "$P/*/Default*.java" -m 10 -o json)"; assert_rc V36b "search with a glob exits 0" 0 "$rc"
assert_jq V36b "glob mode, every hit in DefaultPriceCalculator.java" "$(J V36b)" '.filters.path_mode == "glob" and (.results|length) > 0 and all(.results[]; .file == $f)' --arg f "$P/service/DefaultPriceCalculator.java"
rc="$(cli V37a -- callers tItem --no-lsp -o json)"; assert_rc V37a "callers with a substring exits 0" 0 "$rc"
assert_jq V37a "fuzzy tier, getItems is target #1" "$(J V37a)" '.resolution.tier == "fuzzy" and .targets[0].name == "getItems"'
rc="$(cli V37b -- callers ad --no-lsp -o json)"; assert_rc V37b "callers with a 2-character prefix exits 0" 0 "$rc"
assert_jq V37b "name_prefix tier finds addItem" "$(J V37b)" '.resolution.tier == "name_prefix" and ([.targets[].name] | index("addItem") != null)'
rc="$(cli V37c -- callers 'addItem(Item' --no-lsp -o json)"; assert_rc V37c "callers with a signature prefix exits 0" 0 "$rc"
assert_jq V37c "signature_prefix tier, addItem" "$(J V37c)" '.resolution.tier == "signature_prefix" and [.targets[].name] == ["addItem"]'
rc="$(cli V37d -- callers getItems --no-lsp -o json)"; assert_rc V37d "callers with an exact name exits 0" 0 "$rc"
assert_jq V37d "the exact name still wins over fuzzy" "$(J V37d)" '.resolution.tier == "exact_name" and [.targets[].name] == ["getItems"]'
rc="$(cli V38a KNOT_LITE_SEARCH=hybrid -- search "sums unit price times quantity" -o json)"; assert_rc V38a "hybrid search exits 0" 0 "$rc"
assert_jq V38a "search/v2, hybrid, every hit has a cosine and channels" "$(J V38a)" '.schema == "knot-lite/search/v2" and .provenance == "hybrid" and all(.results[]; (.cosine | type) == "number" and (.channels | length) > 0)'
rc="$(cli V38b KNOT_LITE_SEARCH=hybrid -- search "get items" -o json)"; assert_rc V38b "hybrid identifier search exits 0" 0 "$rc"
assert_jq V38b "getItems is rank 1" "$(J V38b)" '.results[0].name == "getItems"'
rc="$(cli V38c -- search "sums unit price times quantity" -o json)"; assert_rc V38c "default search exits 0" 0 "$rc"
assert_jq V38c "vector by default: score equals cosine" "$(J V38c)" '.provenance == "vector" and all(.results[]; .score == .cosine)'
rc="$(cli V39 KNOT_LITE_LSP=off -- explore "$P/model/Order.java" -o json)"; assert_rc V39 "explore with servers off exits 0" 0 "$rc"
assert_jq V39 "lsp off, every count heuristic" "$(J V39)" '.schema == "knot-lite/explore/v2" and .lsp.status == "off" and all(.groups[].entities[]; .refs_provenance == "heuristic")'

# ---- V13 heuristic ------------------------------------------------------------------------------
rc="$(cli V13 -- callers Order.getItems --no-lsp -o json)"; assert_rc V13 "callers --no-lsp exits 0" 0 "$rc"
assert_jq V13 "provenance is heuristic and the notes say so" "$(J V13)" \
  '.targets[0].provenance == "heuristic" and (.notes|length) > 0 and .targets[0].lsp.status == "disabled"'
assert_subset V13 "the heuristic callers contain the direct callers" "$EXACT_GETITEMS" "$(function_set "$(J V13)")"

# ---- V14-V15 live ---------------------------------------------------------------------------------
if lsp_or_skip V14 "live callers" java; then
  live_exact V14a Order.getItems "$EXACT_GETITEMS"
  live_exact V14b PriceCalculator.total "$EXACT_TOTAL"
  live_exact V14c Order.addItem "$EXACT_ADDITEM"
  rc="$(cli V14d -- callers PriceCalculator -o json)"; assert_rc V14d "callers PriceCalculator exits 0" 0 "$rc"
  assert_jq V14d "PriceCalculator is answered live" "$(J V14d)" '.targets[0].provenance == "lsp-live" and .targets[0].lsp.status == "ok"'
  assert_subset V14d "the implementing and using classes are found" "$IFACE_CONTAINS" "$(function_set "$(J V14d)")"
  assert_subset V14d "file-level rows only in the importing and implementing files" "$(file_rows "$(J V14d)")" "$IFACE_FILE_ROWS"
  rc="$(cli V15 -- callers Order.getItems -o table)"; assert_rc V15 "callers (table) exits 0" 0 "$rc"
  text_in V15 "the table marks the answer [lsp-live]" "$(J V15)" '\[lsp-live\]'
  rc="$(cli V40 -- explore "$P/model/Order.java" -o json)"; assert_rc V40 "explore with a live server exits 0" 0 "$rc"
  assert_jq V40 "lsp ok; getItems counted live (total, count)" "$(J V40)" \
    '.lsp.status == "ok" and ([.groups[].entities[] | select(.name == "getItems") | [.refs_provenance, .referenced_by]] == [["lsp-live", 2]])'
fi

# ---- V16 a server that does not resolve ----------------------------------------------------------
rc="$(cli V16 KNOT_LITE_LSP_JAVA=/nonexistent/server -- callers Order.getItems -o json)"; assert_rc V16 "an unresolvable server still exits 0" 0 "$rc"
assert_jq V16 "degrades to heuristic with status no_server" "$(J V16)" '.targets[0].provenance == "heuristic" and .targets[0].lsp.status == "no_server"'
text_in V16 "the diagnostic says it does not resolve to an executable" "$(J V16)" 'does not resolve to an executable'

# A pre-computed target keeps its pre-computed answer after an edit (lsp-indexed outranks the stale-file check,
# P3-D10), so the stale rows run first, while no target has pre-computed data; the pre-computation then runs
# on the edited fixture.
# ---- V20-V22 stale target, re-index --------------------------------------------------------------
# Insert a method before the final brace of Order.java (appending after the class would not compile).
order="$V/$P/model/Order.java"
sed '$d' "$order" > "$order.tmp"
{ cat "$order.tmp"; printf '\n    public int size() {\n        return getItems().size();\n    }\n}\n'; } > "$order"
rm -f "$order.tmp"
if lsp_or_skip V20 "stale target" java; then
  rc="$(cli V20 -- callers Order.getItems -o json)"; assert_rc V20 "callers after an edit exits 0" 0 "$rc"
  assert_jq V20 "the edited file's target is reanchored live" "$(J V20)" '.targets[0].lsp.status == "ok" and .targets[0].provenance == "lsp-live" and .targets[0].lsp.reanchored == true'
  text_in V20 "a note says target was re-anchored" "$(J V20)" 're-anchored'
  cp "$order" "$order.bak"
  grep -v 'public List<Item> getItems()' "$order.bak" > "$order"
  rc="$(cli V44 -- callers Order.getItems -o json)"; assert_rc V44 "callers with deleted declaration exits 0" 0 "$rc"
  assert_jq V44 "target declaration gone is stale_target" "$(J V44)" '.targets[0].lsp.status == "stale_target" and .targets[0].provenance == "heuristic"'
  mv "$order.bak" "$order"
  rc="$(cli V41 -- explore "$P/model/Order.java" -o json)"; assert_rc V41 "explore of the edited file exits 0" 0 "$rc"
  assert_jq V41 "outline from the server; size listed, not indexed" "$(J V41)" \
    '.outline.provenance == "lsp-live" and .outline.reason == "changed_since_index" and ([.groups[].entities[] | select(.name == "size") | .indexed] == [false])'
  printf 'package com.example.shop.model;\n\npublic class Extra {\n    public int one() {\n        return 1;\n    }\n}\n' > "$V/$P/model/Extra.java"
  rc="$(cli V42 -- explore "$P/model/Extra.java" -o json)"; assert_rc V42 "explore of a file not in the index exits 0" 0 "$rc"
  assert_jq V42 "outline from the server, reason not_indexed" "$(J V42)" \
    '.resolution == "not_indexed" and .outline.reason == "not_indexed" and ([.groups[].entities[].name] | index("Extra") != null)'
  rm -f "$V/$P/model/Extra.java"
  rc="$(cli V21a -- index -o json)"; assert_rc V21a "re-index exits 0" 0 "$rc"
  # jdtls updates its index in the background, so the first live answer after an edit can still describe the
  # file as it was (measured: the call hierarchy missed the new caller in 6 of 8 sessions and the merged answer
  # in 3 of 8; the next session was complete every time). Up to three attempts, each a fresh
  # server; the row states how many it took (README Limitations).
  attempt=0
  while :; do
    attempt=$((attempt + 1))
    rc="$(cli V21b -- callers Order.getItems -o json)"
    jq -e -n --argjson e "$EXACT_GETITEMS_AFTER_EDIT" --argjson a "$(function_set "$(J V21b)")" '($e | sort) == ($a | sort)' >/dev/null 2>&1 && break
    [ "$attempt" -ge 3 ] && break
  done
  assert_rc V21b "callers after the re-index exits 0" 0 "$rc"
  assert_jq V21b "answered live again" "$(J V21b)" '.targets[0].provenance == "lsp-live"'
  assert_set_eq V21b "the new caller is found (attempt $attempt of 3)" "$EXACT_GETITEMS_AFTER_EDIT" "$(function_set "$(J V21b)")"
  if [ "$attempt" -gt 1 ]; then pass V21b "note: the first live answer after the edit was stale, attempt $attempt was complete"; fi
  rc="$(cli V22 -- status --probe-lsp -o json)"; assert_rc V22 "status --probe-lsp exits 0" 0 "$rc"
  assert_jq V22 "jdtls probes ok" "$(J V22)" '.lsp_probe.java.ok == true and .lsp_probe.java.ready_ms > 0'
else
  rc="$(cli V21a -- index -o json)"; assert_rc V21a "re-index exits 0" 0 "$rc"
fi

# ---- V17-V19 pre-computed ------------------------------------------------------------------------
if lsp_or_skip V17 "pre-computed callers" java; then
  rc="$(cli V17 -- index --lsp-precompute all --lsp-min-fanin 2 -o json)"; assert_rc V17 "index with pre-computation exits 0" 0 "$rc"
  assert_jq V17 "java pre-computed (>= 1 target)" "$(J V17)" '.lsp.languages.java.status == "computed" and .counts.lsp_targets >= 1'
  rc="$(cli V18 KNOT_LITE_LSP=off -- callers Order.getItems -o json)"; assert_rc V18 "callers with servers off exits 0" 0 "$rc"
  assert_jq V18 "answered from the index (lsp-indexed)" "$(J V18)" '.targets[0].provenance == "lsp-indexed"'
  assert_set_eq V18 "the pre-computed set equals the exact set (with the new caller)" "$EXACT_GETITEMS_AFTER_EDIT" "$(function_set "$(J V18)")"
  text_in V18 "a note says when it was pre-computed" "$(J V18)" 'pre-computed at'
  logs_before="$(find "$KNOT_LITE_LSP_CACHE_DIR" -name 'java_*.stderr.log' 2>/dev/null | wc -l | tr -d ' ')"
  rc="$(cli V19 -- index --lsp-precompute all --lsp-min-fanin 2 -o json)"; assert_rc V19 "a repeated index exits 0" 0 "$rc"
  assert_jq V19 "the pre-computation is reused" "$(J V19)" '.lsp.languages.java.status == "reused"'
  logs_after="$(find "$KNOT_LITE_LSP_CACHE_DIR" -name 'java_*.stderr.log' 2>/dev/null | wc -l | tr -d ' ')"
  if [ "$logs_before" = "$logs_after" ]; then pass V19 "no language server was started ($logs_after logs)"; else fail V19 "a server was started" "logs before $logs_before, after $logs_after"; fi
  assert_jq V43 "default selection: heuristic, manifest format 2, nothing probed" "$(J V17)" \
    '.lsp.format == 2 and .lsp.selection == "heuristic" and .lsp.languages.java.probed == 0'
  rc="$(cli V43b -- index --lsp-precompute all --lsp-min-fanin 2 --lsp-selection lsp-refs -o json)"; assert_rc V43b "index with the lsp-refs selection exits 0" 0 "$rc"
  assert_jq V43b "lsp-refs selection recomputes after probing" "$(J V43b)" \
    '.lsp.selection == "lsp-refs" and .lsp.languages.java.status == "computed" and .lsp.languages.java.probed >= 1'
  rc="$(cli V43c -- index --lsp-precompute all --lsp-min-fanin 2 -o json)"; assert_rc V43c "back to the default selection exits 0" 0 "$rc"
  assert_jq V43c "heuristic again" "$(J V43c)" '.lsp.selection == "heuristic" and .lsp.languages.java.status == "computed"'
fi

# ---- V23-V33 MCP ------------------------------------------------------------------------------------
# The MCP rows ask for a target that has no pre-computed data (Order.addItem); the pre-computed main target
# (Order.getItems) is answered from the index and gets its own row (V28b).
MCP_TARGET=Order.addItem
mcp_start "$V" KNOT_LITE_LSP_READY_WAIT_SECS=0
mcp_request 1 server/discover; if mcp_wait 1 20; then pass V23 "server/discover answered"; else fail V23 "no answer to server/discover"; fi
mcp_request 2 tools/list
if mcp_wait 2 20; then
  assert_jq V23 "exactly the five tools" "$E2E_MCP_DIR/2.json" \
    '[.result.tools[].name] | sort == ["explore_file","find_callers","list_files","search_hybrid_context","status"]'
else fail V23 "no answer to tools/list"; fi
mcp_call 3 status '{}'
if mcp_wait 3 30; then
  assert_jq V24 "status: index present, java row present" "$(mcp_structured 3)" '.index.state == "present" and ([.languages[].language] | index("java") != null)'
else fail V24 "no answer to status"; fi
mcp_call 4 list_files '{}'; mcp_call 5 list_files '{"path":"src/"}'
if mcp_wait 4 20 && mcp_wait 5 20; then
  assert_jq V25 "list_files {} lists the sources" "$(mcp_structured 4)" '($want - [.files[].path]) == []' --argjson want "$SRC_FILES"
  assert_jq V25 "list_files src/ only lists src/" "$(mcp_structured 5)" '(.files|length) > 0 and all(.files[]; .path | startswith("src/"))'
else fail V25 "no answer to list_files"; fi
mcp_call 6 explore_file "$(jq -n -c --arg f "$P/service/DiscountPriceCalculator.java" '{file_path:$f}')"
if mcp_wait 6 20; then
  assert_jq V26 "explore_file names DiscountPriceCalculator and total" "$(mcp_structured 6)" '(["DiscountPriceCalculator","total"] - [.groups[].entities[].name]) == []'
  assert_jq V26 "explore_file text names DiscountPriceCalculator" "$E2E_MCP_DIR/6.json" '.result.content[0].text | contains("DiscountPriceCalculator")'
else fail V26 "no answer to explore_file"; fi
mcp_call 7 search_hybrid_context '{"query":"sums unit price times quantity","max_results":5}'
if mcp_wait 7 30; then
  assert_jq V27 "search finds DefaultPriceCalculator or total" "$(mcp_structured 7)" '[.results[].name] | (index("DefaultPriceCalculator") != null or index("total") != null)'
else fail V27 "no answer to search"; fi
mcp_call 8 find_callers "$(jq -n -c --arg e "$MCP_TARGET" '{entity_name:$e}')"
if mcp_wait 8 60; then
  if lsp_available java; then
    sc="$(mcp_structured 8)"
    if jq -e '.targets[0].provenance == "lsp-live"' "$sc" >/dev/null; then pass V28 "first find_callers: already lsp-live (fast server)"
    elif jq -e '.targets[0].lsp.status == "not_ready" and ((.notes|join(" ")) | contains("is still starting"))' "$sc" >/dev/null; then pass V28 "first find_callers: not_ready, the note says the server is still starting"
    else fail V28 "first find_callers is neither live nor not_ready" "file: $sc"; fi
    assert_jq V28 "the first answer is not an error" "$E2E_MCP_DIR/8.json" '.result.isError != true'
  else lsp_or_skip V28 "first find_callers" java || true; fi
else fail V28 "no answer to the first find_callers"; fi
if lsp_available java; then
  mcp_call 9 find_callers '{"entity_name":"Order.getItems"}'
  if mcp_wait 9 60; then
    assert_jq V28b "the pre-computed main target is answered from the index" "$(mcp_structured 9)" \
      '.targets[0].provenance == "lsp-indexed" and .targets[0].lsp.status == "indexed"'
  else fail V28b "no answer for the pre-computed main target"; fi
fi
if lsp_or_skip V29 "MCP live callers" java; then
  if mcp_wait_live "$MCP_TARGET" "$LSP_TIMEOUT"; then
    sc="$E2E_LAST_SC"
    pass V29 "find_callers became lsp-live after ${E2E_LIVE_SECS} s"
    assert_set_eq V30 "MCP set equals the CLI set" "$(function_set "$(J V14c)")" "$(function_set "$sc")"
    mcp_call 30 status '{}'; mcp_wait 30 30 || true
    assert_jq V32 "status shows a ready jdtls with pid and memory" "$(mcp_structured 30)" \
      '[.languages[] | select(.language == "java") | .lsp_runtime] | .[0] | .state == "ready" and .pid > 0 and .rss_mib > 0'
  else fail V29 "find_callers never became lsp-live in ${LSP_TIMEOUT} s" "last: ${E2E_LAST_SC:-none}"; fi
fi
mcp_call 40 find_callers '{"entity_name":"x","repo_name":"other"}'; mcp_call 41 nope '{}'
if mcp_wait 40 20 && mcp_wait 41 20; then
  assert_jq V31 "another repository is an error that says so" "$E2E_MCP_DIR/40.json" '.result.isError == true and (.result.content[0].text | contains("not served here"))'
  assert_jq V31 "an unknown tool is an error" "$E2E_MCP_DIR/41.json" '.result.isError == true and (.result.content[0].text | contains("Unknown tool: nope"))'
else fail V31 "no answer to the error probes"; fi
if mcp_stop 30; then pass V33 "the MCP server exits by itself after EOF"; else fail V33 "the MCP server had to be killed"; fi
if jq -e . "$E2E_MCP_DIR/out.jsonl" >/dev/null 2>&1; then pass V33 "every stdout line is valid JSON"; else fail V33 "stdout holds a non-JSON line" "file: $E2E_MCP_DIR/out.jsonl"; fi

# ---- V34-V35 containment and hygiene ---------------------------------------------------------------
# jdtls writes no build or IDE file. It does create the two empty output directories of a Maven module
# (target/classes, target/test-classes): reported, not failed (Appendix D, README Security).
stray="$( { find "$V" \( -name .project -o -name .classpath -o -name .settings \) -not -path "$V/.knot-lite/*"
            find "$V" -path "$V/.knot-lite" -prune -o -path '*/target/*' -type f -print; } 2>/dev/null | head -5)"
if [ -z "$stray" ]; then pass V34 "no file written by jdtls (no .project, .classpath, .settings, nothing under target/)"
else fail V34 "jdtls wrote build or IDE files into the repository" "$stray"; fi
dirs="$(find "$V" -path "$V/.knot-lite" -prune -o -name target -type d -print 2>/dev/null | head -3)"
if [ -n "$dirs" ]; then pass V34 "note: jdtls created only empty output directories under target/"; fi
fixtures_unchanged V34
assert_no_orphans V35 10

# =============================================================================================
# Gradle sub-suite: a Gradle daemon started on behalf of jdtls never outlives it.
# =============================================================================================
G="$(prepare_work java-gradle)"
use_repo "$G"

# gradle_scenario_ready <id>: the check below is meaningful only when Gradle really runs.
gradle_seen() { [ -n "$(marked_gradle_daemons)" ]; }

if gradle_or_skip G01 "Gradle containment"; then
  rc="$(cli G01 -- index --lsp-precompute none -o json)"; assert_rc G01 "index of the Gradle project exits 0" 0 "$rc"

  # G02: one CLI query; a sampler watches for the daemon (its mark is the gradle-owner- path).
  sampler="$E2E_RUN/gradle-sampler.txt"; : > "$sampler"
  ( while :; do marked_gradle_daemons | wc -l | tr -d ' ' >> "$sampler"; sleep 0.5; done ) &
  sampler_pid=$!
  rc="$(cli G02 -- callers Greeter.greet -o json)"
  sleep 10   # G03 below looks at the state 10 s after the CLI exited
  kill "$sampler_pid" 2>/dev/null || true; wait "$sampler_pid" 2>/dev/null || true
  assert_rc G02 "callers Greeter.greet exits 0" 0 "$rc"
  assert_jq G02 "answered live" "$(J G02)" '.targets[0].provenance == "lsp-live" and .targets[0].lsp.status == "ok"'
  assert_set_eq G02 "the only caller is App.main" '["src/main/java/com/example/gradle/App.java#main"]' "$(function_set "$(J G02)")"
  if [ -n "$(awk '$1 > 0' "$sampler")" ]; then pass G02 "a Gradle daemon carrying the ownership mark was seen"
  else fail G02 "Gradle did not run: the containment check was never exercised" "sampler: $sampler"; fi
  assert_no_orphans G03 5

  # G04: MCP idle stop (5 s idle + the 30 s reaper round).
  force_gradle_sync "$G"
  mcp_start "$G" KNOT_LITE_LSP_IDLE_SECS=5
  if mcp_wait_live Greeter.greet "$LSP_TIMEOUT"; then
    if gradle_seen; then pass G04 "a marked Gradle daemon exists while the server runs"; else fail G04 "Gradle did not run: no marked daemon while the server runs"; fi
    n=0; state=""
    while [ "$n" -lt 25 ]; do
      E2E_MCP_SEQ=$((E2E_MCP_SEQ + 1)); sid=$E2E_MCP_SEQ   # ids are unique within a session
      mcp_call "$sid" status '{}'; mcp_wait "$sid" 30 || break
      state="$(jq -r '[.result.structuredContent.languages[] | select(.language == "java") | .lsp_runtime.state] | .[0] // ""' "$E2E_MCP_DIR/$sid.json")"
      [ "$state" = "not_started" ] && break
      sleep 2; n=$((n + 1))
    done
    if [ "$state" = "not_started" ]; then pass G04 "the idle reaper stopped Java (status: not_started)"; else fail G04 "Java was not stopped after the idle time" "state: $state"; fi
    assert_no_orphans G04 20
    if kill -0 "$E2E_MCP_PID" 2>/dev/null; then pass G04 "the MCP server is still running"; else fail G04 "the MCP server died"; fi
  else fail G04 "find_callers never became lsp-live in ${LSP_TIMEOUT} s" "last: ${E2E_LAST_SC:-none}"; fi
  mcp_stop 20 || true

  # G05: closing stdin stops everything.
  force_gradle_sync "$G"
  mcp_start "$G"
  if mcp_wait_live Greeter.greet "$LSP_TIMEOUT"; then
    if gradle_seen; then pass G05 "a marked Gradle daemon exists while the server runs"; else fail G05 "Gradle did not run: no marked daemon while the server runs"; fi
    if mcp_stop 20; then pass G05 "the MCP server exits by itself after EOF"; else fail G05 "the MCP server had to be killed"; fi
    assert_no_orphans G05 20
  else fail G05 "find_callers never became lsp-live in ${LSP_TIMEOUT} s" "last: ${E2E_LAST_SC:-none}"; mcp_stop 5 || true; fi

  # G06: kill -9 leaves the daemon behind (the leak); the next start sweeps it.
  force_gradle_sync "$G"
  mcp_start "$G"
  if mcp_wait_live Greeter.greet "$LSP_TIMEOUT"; then
    if gradle_seen; then pass G06 "a marked Gradle daemon exists before the kill"; else fail G06 "Gradle did not run: no marked daemon before the kill"; fi
    kill -KILL "$E2E_MCP_PID" 2>/dev/null || true
    mcp_stop 5 || true
    sleep 2
    if [ -n "$(owned_procs)" ]; then pass G06 "marked processes outlive a killed MCP server (the leak scenario exists)"
    else fail G06 "nothing outlived the killed MCP server: the sweep check would prove nothing"; fi
    rc="$(cli G06c -- callers Greeter.greet -o json)"; assert_rc G06c "the next start (a CLI query) exits 0" 0 "$rc"
    assert_no_orphans G06 20
  else fail G06 "find_callers never became lsp-live in ${LSP_TIMEOUT} s" "last: ${E2E_LAST_SC:-none}"; mcp_stop 5 || true; fi

  # G07: the work copy may hold .gradle/; the committed fixture must not have changed.
  fixtures_unchanged G07
fi

e2e_finish
