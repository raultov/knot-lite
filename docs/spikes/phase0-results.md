# knot-rag Phase 0 — Feasibility spike results

> **Owner (human):** Raul Tovar · **Repository:** `knot-rag` · **Spike crate:** `docs/spikes/phase0`
> **Date:** 2026-10-03 → 2026-10-05 · **Plan:** [`docs/PLAN_PHASE0_SPIKES_0.md`](PLAN_PHASE0_SPIKES_0.md)
> Every table below is rendered by `summarize` from the JSON artefacts in `docs/spikes/phase0/results/`;
> the narrative is the executing agent's. Cells are traceable to JSON fields (footnote under each table).

---

## 1. Summary and verdict

<!-- BEGIN:table:verdict -->

| ID | Criterion | Threshold | Measured | Verdict |
|---|---|---|---|---|
| G1 | LSP recall (micro) — rust | ≥ 90 % | 100.0% | ✅ |
| G2 | LSP recall — ts | ≥ 90 % | 97.0% | ✅ |
| G3 | LSP recall — java | ≥ 85 % | 95.0% | ✅ |
| G4 | LSP precision — each language | ≥ 90 % | rust 100.0%, ts 100.0%, java 94.1% | ✅ |
| G5 | Cold initialize→ready — rust | ≤ 60 s | 12 s | ✅ |
| G6 | Cold initialize→ready — ts | ≤ 30 s | 2.7 s | ✅ |
| G7 | Cold initialize→ready — java | ≤ 300 s | 20 s | ✅ |
| G8 | Warm p95 per operation (all langs) | ≤ 500 ms | rust 695 ms, ts 189 ms, java 8713 ms | ❌ |
| G9 | Peak RSS (informative) | report | rust 3322.6 MiB, ts 890.4 MiB, java 2737.4 MiB | — |
| G10 | Name-column heuristic accuracy | ≥ 98 % | rust 100.0%, ts 99.4%, java 100.0% | ✅ |
| G11 | int8 overlap@10 (i8pv) vs f32 | ≥ 95 % | knot 100.0%, knot-site 100.0%, spring-ai 100.0% | ✅ |
| G12 | Spike i8pv hit@5 vs knot hit@5 | ≥ 80 % of knot | knot 75.0% vs 75.0%, knot-site 80.0% vs 100.0%, spring-ai 83.3% vs 83.3% | ✅ |
| G13 | Tier-1 file hit@5 (chunk/file domain) | ≥ 80 % | knot 100.0%, knot-site n/a, spring-ai 100.0% | ⚠️ |
| G14 | Load time knot index (best text variant) | ≤ 200 ms | 84 ms | ✅ |
| G15 | Heuristic (tier 2) recall/precision | report only | rust 93.3%/86.5%, ts 92.8%/97.0%, java 88.1%/54.4% | — |
| G16 | Degradation path (both scenarios) | exit 0 + diagnostics + fallback | 6/6 runs | ✅ |

Sources: `gold_*.json`, `s1_*_cold.json`, `s1_*_warm.json`, `s2_*.json`, `s1_*_nolsp*.json`.

<!-- END:table:verdict -->

**Findings.**

- **LSP is a viable default graph backend for all three languages.** Recall 100 % (rust) / 97 % (ts)
  / 95 % (java) and precision 100 % / 100 % / 94.1 % against an adjudicated gold set of 1 184 / 66 /
  8 100 containers. The gates G1–G4 pass everywhere; the kill criteria are not triggered.
- **knot's Neo4j answer set is not a clean gold standard.** Its live index carries stale rows
  (5 606 vs 4 809 clean entities in knot) and its parser/resolver attributes references to
  enclosing or sibling entities (`mod x;` declarations, object literals, classes owning nested
  methods' sites) and to different overloads — 66 % / 95 % / 54 % recall at 93 % / 93 % / 88 %
  precision once adjudicated.
- **The level-2 heuristic (knot's resolver in-process) reproduces knot's Neo4j counts exactly**
  (38 calls for `resolve_reference_intents`, 661+1 450 for `Prompt`) at 93 % precision, and needs
  no database. It is the tier-2 fallback of record; its cost is resolve-time only (0.9 s rust /
  41 s java), precomputable at index time.
- **Latency (G8) is the one failing gate.** Warm p95 reaches 695 ms (rust `prepareCallHierarchy`)
  and 8 713 ms (java `incomingCalls` on high-fan-in targets). The tier rule still grants
  "LSP default" per language (G1–G4 + cold start pass), but the numbers argue for pre-computing
  edges at index time for wide targets and keeping live LSP for the rest.
- **File-based vectors work and int8 is free.** `i8pv_b64` keeps top-10 overlap at 100 % on all
  three repos and is 6–20× smaller than `f32_dec`; per-file shards add ≤ 3 KiB to a git pack on
  append. But the §20.2 commit rule **fails on pack size** (25 % of the source pack vs the 10 %
  target) and on re-index byte-identity (knot-side UUID churn) → `.gitignore` + opt-in.
- **Two knot-side data-model findings** were surfaced by the determinism/parity runs: HTML/CSS
  entities get `Uuid::new_v4()` ids (random per run) and the v5 UUID drops kind/name, so
  `constant`+`function` pairs collide. The shard format itself is deterministic (0 differing
  vector records, max |Δ| = 0 across reruns/batching/threads) and parity with Qdrant is exact
  (cosine 1.000000 on 531 sampled ids).
- **The degradation path is honest**: with no LSP binary the probe reports a human-readable
  diagnostic with the install hint, switches to tier 2 and answers every target in ≤ 18 ms
  (`provenance: "heuristic"`); a server that dies before `initialize` is diagnosed with its exit
  status.

**Decisions (§20.2), as evaluated against the measurements:**

- **Committed-index default: `gitignore` + opt-in flag.** All three repos fail the "pack ≤ 10 % of
  source pack" criterion (best 25 % with `i8pv_b64`/`i8fx_b64`) and "re-index is byte-identical"
  (UUID churn). The append-churn criterion passes (1–3 KiB). Majority + spring-ai tie-break all
  point the same way. *Proposed threshold change for the human:* 10 % → ~25 % would admit the
  int8 variants on knot-site and spring-ai (knot's source pack compresses unusually well, so its
  ratio is inflated at 2.34×). Criterion 2 is a knot-side bug class, not a shard-format property.
- **Vector encoding: `i8pv_b64` recommended** (100 % overlap@10 everywhere, deterministic under
  batching). `i8fx_b64` is the *smallest* variant that satisfies G11 (95.0 % on spring-ai, clip
  rate 0) and is ~4 % smaller; it is the pick only if size dominates over the 5 % overlap margin.
- **Per-language graph tier: "LSP default" for rust, ts and java** (G1–G4 + cold-start gates all
  pass). With the G8 latency caveat: pre-compute LSP edges at index time for high-fan-in targets
  and use live LSP for the rest (Phase 2 decision).
- **Kill criteria: not triggered** — no two languages fail G1–G4.

---

## 2. Environment

<!-- BEGIN:table:environment -->

| Item | Value | Source |
|---|---|---|
| rust-analyzer / ts server / jdtls | rust-analyzer 1.98.1 · typescript-language-server 5.3.0 · jdtls 1.61.0 | s1_*.server.version |
| Rust / JDK | rustc 1.98.1 (2026-09-01) · OpenJDK 21.0.12.1 · Maven 3.9.7 | results/raw/env.txt |
| knot | knot CLI 1.11.0 · knot crate (path) 1.11.1 | results/raw/env.txt |
| embedding model | AllMiniLML6V2, 384-dim, symmetric (no query/passage prefix) | s2_*.model |
| machine | Linux 7.0.0-34-generic · 32 cores · 61 GB RAM · /tmp tmpfs | plan §4 |
| snapshot fingerprints | knot `8b6c9173113e`, knot-site `567823fe8e62`, spring-ai `c4d9728f8e14` | s1_*.snapshot_fingerprint |
| S1 wall time (cold / warm) | rust 12 s / 5.0 s, ts 2.9 s / 2.8 s, java 23 s / 6.4 s | s1_*.timings_ms |
| S2 index wall time | knot 229 s, knot-site 40 s, spring-ai 1287 s | s2_*.timings_ms |

<!-- END:table:environment -->

Runs were executed sequentially (32 cores idle otherwise). The sandbox is
`/tmp/knot-rag-spike/work/` (rsync of `~/.knot/repos/<id>`); language servers only ever touched
the sandbox or `/tmp/knot-rag-spike/{target-ra,jdtls-ws,logs}`. Dates: S1 2026-10-03/04,
S2 2026-10-04/05.

> **Build note (deviation, T11):** the measurement binaries ran **debug**-built (the plan's T1.1
> note asked for `--release`). This only affects the spike's own client-side figures — the S2
> `load_ms`/`scan_ms` and the parse/prepare timings. LSP latencies (G8) are server-dominated and
> all byte-level results (sizes, determinism, parity, gold) are build-independent. Release
> builds would only improve the client-side numbers; every gate passes with margin today, and
> re-running the full matrix (≈ 3 h, spring-ai determinism alone ≈ 88 min) on one consistent
> build was judged not worth the risk of mixing artefacts this late in the spike.

---

## 3. S1 — LSP capability matrix

<!-- BEGIN:table:s1_capabilities -->

| Language | call hierarchy | type hierarchy | implementation | references | documentSymbol | workspaceSymbol | server requests seen |
|---|---|---|---|---|---|---|---|
| rust | ✅ | ❌ | ✅ | ✅ | ✅ | ✅ | window/workDoneProgress/create |
| ts | ✅ | ❌ | ✅ | ✅ | ✅ | ✅ | window/workDoneProgress/create, workspace/configuration |
| java | ✅ | ❌ | ✅ | ✅ | ✅ | ✅ |  |

Sources: `s1_*_warm.json` (`capabilities`, `server_requests_seen`). Type hierarchy is always `false`: lsp-types 0.95 has no `type_hierarchy_provider` field to read.

<!-- END:table:s1_capabilities -->

jdtls additionally speaks `workspace/configuration`, `client/registerCapability` and
`window/showMessageRequest` (all answered by the client router); tsserver only asks for
`window/workDoneProgress/create`; rust-analyzer asks for nothing beyond progress. The custom
notifications `experimental/serverStatus` (rust-analyzer) and `language/status` (jdtls) drive
readiness; tsserver is detected by `$/progress` quiescence.

---

## 4. S1 — Start-up, memory, latency

<!-- BEGIN:table:s1_startup -->

| Run | spawn→init (ms) | init→ready (ms) | first request (ms) | prepare p50/p95 | calls p50/p95 | refs p50/p95 | impl p50/p95 | peak RSS | procs |
|---|---|---|---|---|---|---|---|---|---|
| rust cold | 20 | 12444 | 4.0 | 1.0/704 | 37/231 | 2.0/478 | 0.0/0.0 | 4458.5 MiB | 51 |
| rust warm | 21 | 4937 | 3.0 | 1.0/695 | 52/220 | 2.0/487 | 0.0/0.0 | 3322.6 MiB | 3 |
| ts cold | 109 | 2744 | 31 | 0.0/364 | 2.0/28 | 3.0/30 | 2.0/23 | 915.0 MiB | 6 |
| ts warm | 112 | 2727 | 39 | 0.0/189 | 1.0/28 | 2.0/11 | 1.0/2.0 | 890.4 MiB | 6 |
| java cold | 3335 | 19956 | 11 | 2.0/13 | 1959/22416 | 1181/8549 | 271/762 | 3624.3 MiB | 1 |
| java warm | 3924 | 2477 | 7.0 | 2.0/5.0 | 1441/8713 | 1232/3329 | 268/646 | 2737.4 MiB | 1 |

Sources: `s1_*_{cold,warm}.json` (`timings_ms`, `latency_ms`, `rss`). The java cold row is the plan-conformant cold run (jdtls workspace wiped, warm `~/.m2`); the first ever cold run measured init→ready = 171 000 ms.

<!-- END:table:s1_startup -->

Cold `initialize→ready` is comfortably inside the gates (12 s / 2.7 s / 20 s). Peak RSS
(3.0–4.5 GiB for rust-analyzer and jdtls, 0.9 GiB for tsserver) is dominated by the language
servers themselves. The p95 latency row is where the single failing gate lives (G8): rust's
`prepareCallHierarchy` p95 is 695 ms (one lazy first hit) and jdtls `incomingCalls` p95 reaches
8.7 s on high-fan-in Java targets.

---

## 5. S1 — Accuracy

<!-- BEGIN:table:s1_accuracy -->

| Lang | System | micro P | micro R | macro P | macro R | tp | fp | fn | file P | file R |
|---|---|---|---|---|---|---|---|---|---|---|
| rust | lsp | 100.0% | 100.0% | 100.0% | 100.0% | 1184 | 0 | 0 | 73.8% | 100.0% |
| rust | knot_db | 93.5% | 66.5% | 90.2% | 78.1% | 787 | 55 | 397 | 94.6% | 92.8% |
| rust | heuristic | 93.3% | 86.5% | 91.8% | 84.4% | 1024 | 73 | 160 | 74.0% | 97.4% |
| rust | lsp_calls | 100.0% | 11.1% | 100.0% | 60.2% | 131 | 0 | 1053 | 100.0% | 25.0% |
| rust | knot_db_calls | 99.2% | 11.0% | 97.4% | 58.6% | 130 | 1 | 1054 | 100.0% | 24.3% |
| rust | gold size | 1184 |  |  |  |  |  |  |  |  |
| ts | lsp | 100.0% | 97.0% | 100.0% | 99.0% | 64 | 0 | 2 | 100.0% | 100.0% |
| ts | knot_db | 92.6% | 95.5% | 90.8% | 94.7% | 63 | 5 | 3 | 100.0% | 96.0% |
| ts | heuristic | 92.8% | 97.0% | 90.8% | 95.2% | 64 | 5 | 2 | 100.0% | 96.0% |
| ts | lsp_calls | 100.0% | 37.9% | 100.0% | 71.4% | 25 | 0 | 41 | 100.0% | 48.0% |
| ts | knot_db_calls | 82.1% | 34.8% | 86.9% | 66.7% | 23 | 5 | 43 | 100.0% | 44.0% |
| ts | gold size | 66 |  |  |  |  |  |  |  |  |
| java | lsp | 94.1% | 95.0% | 95.5% | 97.4% | 7696 | 482 | 404 | 46.6% | 99.8% |
| java | knot_db | 88.2% | 54.2% | 86.5% | 49.0% | 4391 | 590 | 3709 | 95.7% | 97.3% |
| java | heuristic | 88.1% | 54.4% | 83.7% | 49.5% | 4403 | 597 | 3697 | 95.2% | 98.1% |
| java | lsp_calls | 100.0% | 24.1% | 100.0% | 41.2% | 1953 | 0 | 6147 | 12.8% | 7.9% |
| java | knot_db_calls | 99.9% | 11.3% | 91.7% | 14.0% | 918 | 1 | 7182 | 99.2% | 26.9% |
| java | gold size | 8100 |  |  |  |  |  |  |  |  |

Sources: `gold_*.json` (`systems.*`). `lsp_calls`/`knot_db_calls` are the secondary calls-only metrics; gold = union of the three systems' containers, labelled by adjudication (estimated for rust/java: stratified sample of 50).

<!-- END:table:s1_accuracy -->

<!-- BEGIN:table:s1_accuracy_category -->

| Lang | Category | System | micro P | micro R | tp | fp | fn |
|---|---|---|---|---|---|---|---|
| rust | free_fn | lsp | 100.0% | 100.0% | 98 | 0 | 0 |
| rust | free_fn | knot_db | 100.0% | 90.8% | 89 | 0 | 9 |
| rust | free_fn | heuristic | 100.0% | 90.8% | 89 | 0 | 9 |
| rust | method | lsp | 100.0% | 100.0% | 4 | 0 | 0 |
| rust | method | knot_db | 75.0% | 75.0% | 3 | 1 | 1 |
| rust | method | heuristic | 75.0% | 75.0% | 3 | 1 | 1 |
| rust | trait | lsp | 100.0% | 100.0% | 13 | 0 | 0 |
| rust | trait | knot_db | 45.5% | 38.5% | 5 | 6 | 8 |
| rust | trait | heuristic | 50.0% | 46.2% | 6 | 6 | 7 |
| rust | trait_method | lsp | 100.0% | 100.0% | 38 | 0 | 0 |
| rust | trait_method | knot_db | 100.0% | 100.0% | 38 | 0 | 0 |
| rust | trait_method | heuristic | 100.0% | 100.0% | 38 | 0 | 0 |
| rust | type | lsp | 100.0% | 100.0% | 1031 | 0 | 0 |
| rust | type | knot_db | 93.1% | 63.2% | 652 | 48 | 379 |
| rust | type | heuristic | 93.1% | 86.1% | 888 | 66 | 143 |
| ts | free_fn | lsp | 100.0% | 100.0% | 14 | 0 | 0 |
| ts | free_fn | knot_db | 100.0% | 100.0% | 14 | 0 | 0 |
| ts | free_fn | heuristic | 100.0% | 100.0% | 14 | 0 | 0 |
| ts | homonym | lsp | 100.0% | 100.0% | 7 | 0 | 0 |
| ts | homonym | knot_db | 62.5% | 71.4% | 5 | 3 | 2 |
| ts | homonym | heuristic | 62.5% | 71.4% | 5 | 3 | 2 |
| ts | interface | lsp | 100.0% | 95.1% | 39 | 0 | 2 |
| ts | interface | knot_db | 100.0% | 97.6% | 40 | 0 | 1 |
| ts | interface | heuristic | 100.0% | 100.0% | 41 | 0 | 0 |
| ts | method | lsp | 100.0% | 100.0% | 4 | 0 | 0 |
| ts | method | knot_db | 66.7% | 100.0% | 4 | 2 | 0 |
| ts | method | heuristic | 66.7% | 100.0% | 4 | 2 | 0 |
| java | abstract_class | lsp | 97.8% | 100.0% | 44 | 1 | 0 |
| java | abstract_class | knot_db | 97.8% | 100.0% | 44 | 1 | 0 |
| java | abstract_class | heuristic | 97.8% | 100.0% | 44 | 1 | 0 |
| java | interface | lsp | 83.2% | 92.6% | 2343 | 472 | 187 |
| java | interface | knot_db | 90.3% | 72.2% | 1826 | 196 | 704 |
| java | interface | heuristic | 90.3% | 72.2% | 1826 | 196 | 704 |
| java | interface_method | lsp | 100.0% | 100.0% | 1570 | 0 | 0 |
| java | interface_method | knot_db | 100.0% | 0.1% | 2 | 0 | 1568 |
| java | interface_method | heuristic | 66.7% | 0.9% | 14 | 7 | 1556 |
| java | method | lsp | 99.3% | 99.1% | 426 | 3 | 4 |
| java | method | knot_db | 98.9% | 20.9% | 90 | 1 | 340 |
| java | method | heuristic | 98.9% | 20.9% | 90 | 1 | 340 |
| java | type | lsp | 99.8% | 94.0% | 3313 | 6 | 213 |
| java | type | knot_db | 86.1% | 68.9% | 2429 | 392 | 1097 |
| java | type | heuristic | 86.1% | 68.9% | 2429 | 392 | 1097 |

Sources: `gold_*.json` (`systems.*.by_category`).

<!-- END:table:s1_accuracy_category -->

`lsp_calls` / `knot_db_calls` (calls-only) recall collapses to 11–38 % because type references
dominate the gold — expected, and the reason the primary `lsp` system unions incoming ∪
references ∪ implementation. rust and java metrics are **estimated** (adjudicated stratified
sample of 50 from 470 / 5 192 pending items, seed 42, weights `N/n`); ts is exact (10 items).

---

## 6. S1 — Notable disagreements

<!-- BEGIN:table:s1_disagreements -->

| Lang | Target | Entity | Found by | Verdict | Reason |
|---|---|---|---|---|---|
| rust | rust-12 | `src/pipeline/parser/languages/varnish/mod.rs:5` vtc | heuristic | fp | `mod vtc;` declaration owns no ParsedEntity site; the uses live in varnish/mod.rs:11/24/42 and vtc.rs — mod-declaration attribution |
| rust | rust-13 | `src/models/mod.rs:13` subgraph | knot_db, heuristic | fp | `mod subgraph;` owns no EntityKind site; the `pub use entity::{... EntityKind ...}` at mod.rs:16 is file-level |
| rust | rust-15 | `src/bin/knot.rs:22` main | knot_db, heuristic | fp | main calls `VectorDb::connect` at knot.rs:39 (VectorConnectExt), not the ConnectExt trait |
| rust | rust-16 | `src/cli_tools/search_hybrid_context/mod.rs:29` recall | knot_db, heuristic | fp | `pub mod recall;` owns no RepoScope site; the `use crate::models::RepoScope;` at mod.rs:35 is file-level |
| ts | ts-09 | `src/state/consentStore.ts:1` <module> | knot_db, heuristic, knot_db_calls | fp | emit() sites at lines 36/65 are owned by `settle` and `request`; knot added an edge from the `consentStore` object literal (line 41), which container_at climbs to <module> — no owned site |
| ts | ts-21 | `src/components/Installation.tsx:99` handleTabKeyDown | knot_db, heuristic, knot_db_calls | fp | handleTabKeyDown calls `handleTab(next)` at line 104, not setActiveTab; no setActiveTab site in its span |
| java | java-02 | `spring-ai-client-chat/src/main/java/org/springframework/ai/chat/client/DefaultChatClient.java:111` prompt | heuristic | fp | OVERLOAD RULE: `prompt(String)` delegates to `prompt(new Prompt(content))` — the `prompt(Prompt)` overload, not the no-arg target `prompt()` |
| java | java-03 | `spring-ai-client-chat/src/main/java/org/springframework/ai/chat/client/ChatClient.java:67` create | knot_db, heuristic, knot_db_calls | fp | OVERLOAD RULE: `create(ChatModel, ObservationRegistry)` calls the 4-arg `create(...)`, not the 1-arg target `create(ChatModel)` |
| java | java-10 | `spring-ai-model/src/main/java/org/springframework/ai/tool/StaticToolCallbackProvider.java:1` <module> | lsp | fp | javadoc {@link ToolCallback} / @see ToolCallback mentions in comments — comment-only match, not a code reference |
| java | java-11 | `spring-ai-model/src/main/java/org/springframework/ai/tool/augment/AugmentedToolCallback.java:111` call | heuristic | fp | OVERLOAD RULE: `call(String, ToolContext)` delegates to the 2-arg `delegate.call(...)`, not the 1-arg target `call(String)` |
| java | java-15 | `spring-ai-model/src/main/java/org/springframework/ai/embedding/EmbeddingModel.java:39` EmbeddingModel | lsp | fp | javadoc {@link Document#getText()} mention in the interface comment — comment-only match |
| java | java-17 | `spring-ai-vector-store/src/test/java/org/springframework/ai/vectorstore/observation/AbstractObservationVectorStoreUpsertTests.java:1` <module> | lsp | fp | javadoc {@link AbstractObservationVectorStore} mention in a test comment — comment-only match |
| java | java-17 | `spring-ai-vector-store/src/test/java/org/springframework/ai/vectorstore/observation/AbstractObservationVectorStoreUpsertTests.java:46` AbstractObservationVectorStoreUpsertTests | knot_db, heuristic | fp | the constructor declaration's own name is not a type reference to the class; knot linked the constructor to its class structurally |

Sources: `gold_*.json` (`notable_disagreements`), marked `notable = true` in `results/adjudication/*.toml`.

<!-- END:table:s1_disagreements -->

The recurring patterns: javadoc `{@link}` mentions that jdtls counts as references (the only
LSP-side false positives), calls to a *different overload* attributed to the target, and
enclosing/sibling attributions from knot's parser.

---

## 7. S1 — Name-column heuristic

<!-- BEGIN:table:s1_columns -->

| Lang | compared | matched | accuracy | failed_to_locate | symbol_not_found | files w/o symbols | example mismatch |
|---|---|---|---|---|---|---|---|
| rust | 465 | 465 | 100.0% | 1 | 82 | 6 | .knot-agent.md:1 Knot CLI: Agent Skill Index (symbol_not_found) |
| ts | 178 | 177 | 99.4% | 100 | 78 | 5 | README.md:1 knot-site (symbol_not_found) |
| java | 315 | 315 | 100.0% | 0 | 2 | 5 | GoogleGenAiChatOptions.java:470 AbstractBuilder (symbol_not_found) |

Sources: `s1_*_warm.json` (`column_heuristic`). Compared = entities whose name was located AND matched to a document symbol; `symbol_not_found` are knot entities LSP does not symbolize (test fixtures, CSS, Markdown).

<!-- END:table:s1_columns -->

Overall 957/958 computed positions match the server's `selection_range` (99.9 %) — G10 passes.
The one `differs` case is a minified single-letter identifier in `scripts/fetch-updates.mjs`.
`failed_to_locate` is concentrated in TS (100 of 156): object-literal members and re-exports
where the declaration-name heuristic has no keyword to anchor on. None of the 65 S1 targets is
affected (all located `ok`).

---

## 8. S1 — Degradation (tier 2)

<!-- BEGIN:table:s1_degradation -->

| Lang | Scenario | lsp_available | lsp_started | tier | degrade (ms) | fallback (ms) | graph build (ms) | targets | Diagnostic |
|---|---|---|---|---|---|---|---|---|---|
| rust | missing | ✅ | ✅ | 2 | 0.0 | 2.0 | 1007 | 20 | rust-analyzer not found in PATH (searched 18 dirs). Install it with `rustup component add rust-analyzer`. |
| rust | broken | ❌ | ✅ | 2 | 4.0 | 2.0 | 988 | 20 | LSP server exited with status 1 before initialize (service stopped) |
| ts | missing | ✅ | ✅ | 2 | 0.0 | 0.0 | 41 | 21 | typescript-language-server not found in PATH (searched 18 dirs). Install it with `npm install -g typescript-language-server typescript`. |
| ts | broken | ❌ | ✅ | 2 | 2.0 | 0.0 | 38 | 21 | LSP server exited with status 1 before initialize (service stopped) |
| java | missing | ✅ | ✅ | 2 | 0.0 | 17 | 44392 | 24 | jdtls not found in PATH (searched 18 dirs). Install it with `docs/spikes/phase0/scripts/install_jdtls.sh` (https://download.eclipse.org/jdtls/milestones/). |
| java | broken | ❌ | ✅ | 2 | 2.0 | 18 | 45741 | 24 | LSP server exited with status 1 before initialize (service stopped) |

Sources: `s1_*_nolsp.json`, `s1_*_nolsp_broken.json`. All answers carry `provenance: "heuristic"` (tier 2). `graph build` is the T6 resolver (setup); `fallback` is the answering step asserted ≤ 5 000 ms.

<!-- END:table:s1_degradation -->

Both scenarios exit 0 for all three languages (6/6), every target gets a fallback answer with
`provenance: "heuristic"`, and the real PATH is never modified (the sanitized PATH is passed to
the lookup and the child only). `graph build` is the T6 resolver measured separately; the
asserted ≤ 5 s window applies to the answering step after the degradation decision.

---

## 9. S2 — Sizes

<!-- BEGIN:table:s2_sizes -->

| Repo | Variant | raw | gzip | git pack | pack / source | bytes / record | shards |
|---|---|---|---|---|---|---|---|
| knot | f32_dec | 24.7 MiB | 10.0 MiB | 10.0 MiB | 969.4% | 4962 B | 329 |
| knot | f32_b64 | 11.5 MiB | 8.1 MiB | 8.1 MiB | 789.1% | 2311 B | 329 |
| knot | i8pv_b64 | 3.9 MiB | 2.4 MiB | 2.4 MiB | 234.1% | 791 B | 329 |
| knot | i8fx_b64 | 3.9 MiB | 2.2 MiB | 2.3 MiB | 219.8% | 775 B | 329 |
| knot | nov | 1.3 MiB | 367.8 KiB | 0.0 KiB | 0.0% | 256 B | 329 |
| knot | source tree | 3.7 MiB | 1.0 MiB | 1.0 MiB | — | — | — |
| knot-site | f32_dec | 7.2 MiB | 2.9 MiB | 2.9 MiB | 107.3% | 4913 B | 81 |
| knot-site | f32_b64 | 3.3 MiB | 2.4 MiB | 2.4 MiB | 87.5% | 2262 B | 81 |
| knot-site | i8pv_b64 | 1.1 MiB | 702.0 KiB | 708.0 KiB | 25.5% | 741 B | 81 |
| knot-site | i8fx_b64 | 1.1 MiB | 653.0 KiB | 657.0 KiB | 23.6% | 726 B | 81 |
| knot-site | nov | 309.8 KiB | 91.8 KiB | 0.0 KiB | 0.0% | 207 B | 81 |
| knot-site | source tree | 3.1 MiB | 2.7 MiB | 2.7 MiB | — | — | — |
| spring-ai | f32_dec | 135.0 MiB | 54.9 MiB | 55.2 MiB | 105.9% | 4978 B | 2604 |
| spring-ai | f32_b64 | 63.1 MiB | 44.4 MiB | 44.6 MiB | 85.7% | 2327 B | 2604 |
| spring-ai | i8pv_b64 | 21.9 MiB | 13.1 MiB | 13.5 MiB | 25.8% | 807 B | 2604 |
| spring-ai | i8fx_b64 | 21.5 MiB | 12.3 MiB | 12.6 MiB | 24.2% | 791 B | 2604 |
| spring-ai | nov | 7.4 MiB | 2.1 MiB | 0.0 KiB | 0.0% | 272 B | 2604 |
| spring-ai | source tree | 87.4 MiB | 59.7 MiB | 52.1 MiB | — | — | — |

Sources: `s2_*.json` (`sizes`, `git.pack_kib`, `git.ratio_vs_source`). gzip = per-file flate2 default level, summed (approximates loose objects).

<!-- END:table:s2_sizes -->

---

## 10. S2 — Determinism and churn

<!-- BEGIN:table:s2_determinism -->

| Repo | Variant | rerun identical | batching identical | threads identical | vector records differing | max |Δ| | id mismatches | append Δpack | prepend Δpack |
|---|---|---|---|---|---|---|---|---|---|
| knot | f32_dec | ❌ | ❌ | ❌ | 0 | 0.00e0 | 133 | 2 KiB | 47 KiB |
| knot | f32_b64 | ❌ | ❌ | ❌ | 0 | 0.00e0 | 133 | 2 KiB | 39 KiB |
| knot | i8pv_b64 | ❌ | ❌ | ❌ | 0 | 0.00e0 | 133 | 1 KiB | 12 KiB |
| knot | i8fx_b64 | ❌ | ❌ | ❌ | 0 | 0.00e0 | 133 | 1 KiB | 11 KiB |
| knot | nov | ❌ | ❌ | ❌ | 0 | 0.00e0 | 133 |  |  |
| knot-site | f32_dec | ❌ | ❌ | ❌ | 0 | 0.00e0 | 275 | 2 KiB | 32 KiB |
| knot-site | f32_b64 | ❌ | ❌ | ❌ | 0 | 0.00e0 | 275 | 2 KiB | 26 KiB |
| knot-site | i8pv_b64 | ❌ | ❌ | ❌ | 0 | 0.00e0 | 275 | 1 KiB | 7 KiB |
| knot-site | i8fx_b64 | ❌ | ❌ | ❌ | 0 | 0.00e0 | 275 | 1 KiB | 8 KiB |
| knot-site | nov | ❌ | ❌ | ❌ | 0 | 0.00e0 | 275 |  |  |
| spring-ai | f32_dec | ❌ | ❌ | ❌ | 0 | 0.00e0 | 1 | 3 KiB | 55 KiB |
| spring-ai | f32_b64 | ❌ | ❌ | ❌ | 0 | 0.00e0 | 1 | 3 KiB | 45 KiB |
| spring-ai | i8pv_b64 | ❌ | ❌ | ❌ | 0 | 0.00e0 | 1 | 2 KiB | 14 KiB |
| spring-ai | i8fx_b64 | ❌ | ❌ | ❌ | 0 | 0.00e0 | 1 | 2 KiB | 14 KiB |
| spring-ai | nov | ❌ | ❌ | ❌ | 0 | 0.00e0 | 1 |  |  |

Sources: `s2_*.json` (`determinism`, `git.churn`). Byte-identity fails only because knot's html parser emits `Uuid::new_v4()` ids (`id mismatches`); vectors are identical (0 differing records, max |Δ| = 0.0). Churn: `append` adds one function, `prepend` shifts all lines (every UUID of the file changes).

<!-- END:table:s2_determinism -->

---

## 11. S2 — Performance

<!-- BEGIN:table:s2_performance -->

| Repo | load f32_dec (ms) | load f32_b64 (ms) | load i8pv (ms) | load i8fx (ms) | load cache (ms) | embed p50 (ms) | scan p50 (ms) | scan p95 (ms) | rows |
|---|---|---|---|---|---|---|---|---|---|
| knot | 720 | 201 | 87 | 84 | 59 | 3.5 | 23 | 25 | 5216 |
| knot-site | 210 | 59 | 25 | 24 | 17 | 3.7 | 7.3 | 8.4 | 1536 |
| spring-ai | 4039 | 1142 | 516 | 507 | 350 | 4.5 | 134 | 139 | 28426 |

Sources: `s2_*.json` (`load_ms`, `query`, `counts`). Load = read+parse into an f32 matrix, median of 5; scan = brute-force top-10 over the entity domain, median of 20.

<!-- END:table:s2_performance -->

---

## 12. S2 — Quality

<!-- BEGIN:table:s2_quality -->

| Repo | hit@5 f32 | hit@5 i8pv | hit@5 i8fx | hit@5 knot | hit@10 f32 | hit@10 knot | overlap@10 vs knot | i8pv overlap@10 | i8fx overlap@10 | i8fx clip | Qdrant parity (found, cos) |
|---|---|---|---|---|---|---|---|---|---|---|---|
| knot | 75.0% | 75.0% | 75.0% | 75.0% | 75.0% | 75.0% | 36.2% | 100.0% | 98.8% | 0.0% | 180/180 · 1.000000 |
| knot-site | 80.0% | 80.0% | 80.0% | 100.0% | 80.0% | 100.0% | 44.0% | 100.0% | 100.0% | 0.0% | 156/156 · 1.000000 |
| spring-ai | 83.3% | 83.3% | 83.3% | 83.3% | 83.3% | 100.0% | 38.3% | 100.0% | 95.0% | 0.0% | 195/195 · 1.000000 |

Sources: `s2_*.json` (`quality`, `quantization`, `parity_with_qdrant`). Search domain = entities only. knot = `knot search` top-10 from `groundtruth/search_*.json` (hybrid re-ranked, informative overlap only).

<!-- END:table:s2_quality -->

---

## 13. S2 — Tier 1

<!-- BEGIN:table:s2_tier1 -->

| Repo | queries | hit@1 | hit@5 | hit@5 mixed | tier-1 files | chunks | file vectors | size increase |
|---|---|---|---|---|---|---|---|---|
| knot | 9 | 88.9% | 100.0% | 55.6% | 60 | 347 | 60 | 662.1% |
| knot-site | 0 | 0.0% | 0.0% | 0.0% | 6 | 10 | 6 | 230.4% |
| spring-ai | 2 | 50.0% | 100.0% | 50.0% | 114 | 572 | 114 | 154.3% |

Sources: `s2_*.json` (`tier1`, `counts`). hit@5 collapses results to files (best score per file) over chunk+file vectors; `mixed` lets tier-1 files compete with code entities. knot-site has no `level1` queries (empty domain → 0.00 is an artifact).

<!-- END:table:s2_tier1 -->

---

## 14. Decisions

1. **Committed-index default (§20.2): `.gitignore` + opt-in.** Majority rule + spring-ai
   tie-break: all three repos fail the 10 % pack criterion (25 % with int8) and the
   re-index-byte-identical criterion (knot-side UUID churn). Appendix F's index layout stands;
   only the *default* changes. Revisit after the UUID findings are fixed (below).
2. **Encoding: `i8pv_b64`.** 100 % overlap@10 on all repos, deterministic under batching and
   thread counts, 2.4–13.5 MiB gzip for 5 k–28 k entities. `i8fx_b64` is the smallest G11
   pass (95.0 % on the largest repo, zero clipping) if size dominates.
3. **Per-language tier: LSP default × 3** (G1–G4 + cold gates pass). Add a Phase 2
   pre-computed-edge path for high-fan-in targets (jdtls `incomingCalls` p95 8.7 s).
4. **Kill criteria: not triggered.** No two languages fail G1–G4.
5. **knot-side fixes to request upstream** (they distort two measurements):
   `html.rs`/`css.rs` `Uuid::new_v4()` → the documented v5 scheme; v5 UUID to include kind or
   name (14 collisions in knot-site's 1 520 entities).
6. **Tier-1 chunks live** (T9.3): 60-line windows + per-file vectors answer file-location queries
   at hit@5 = 1.00 on knot and spring-ai; keep them in the index with `kind: chunk|file` records.

## 15. Deviations from plan

Recorded in full at the head of each executed section of
[`PLAN_PHASE0_SPIKES_0.md`](PLAN_PHASE0_SPIKES_0.md); in summary:

- **T2:** `Type.member` knot queries corrected to Rust `::`/bare forms; rust-19 adjudicated to
  `GraphDb::connect` (knot extracts no trait-method entities); java-09 moved to
  `VectorStoreRetriever.java:45` (the plan's file has no `similaritySearch`); the real callers
  JSON carries a `resolution` metadata object.
- **T3:** `parse_files_stream` + concurrent drain instead of `parse_files` (that wrapper deadlocks
  > 1024 entities); `union` added to the container markers; entity counts vs knot explained by
  stale index rows (config files proven off).
- **T4:** `RequestOutcome` struct instead of a tuple; `#[expect(deprecated)]` on
  `InitializeParams::root_uri` (jdtls/tsserver still read it — reported to the human);
  sysinfo `without_tasks()` or the RSS sampler counts threads.
- **T5:** `src/probe.rs` holds the S1 schema+flow; `self_count` and two column-check counters
  added; column check skips symbol-less files and matches signature-suffixed symbol names
  (`foo()` ≡ `foo`); `type_hierarchy` reads false (lsp-types 0.95 has no capability field);
  java "cold" is plan-conformant (jdtls ws wiped) — the first-ever cold run was 171 s.
- **T7:** the gold rule is "the container **owns** a genuine reference site" (documented in every
  verdict) — the reading under which G1–G3 are achievable by any LSP; stratified sampling for
  rust/java (> 150 pending), `notable = true` markers for the disagreements table.
- **T8:** the 5 s assertion applies to the post-degradation answering step (the tier-2 graph is
  built in setup, as in production where it is precomputed at index time).
- **T9:** `flate2` dependency added (gzip sizes); `--only-file`/`--ids-out` sub-flags; Qdrant
  parity correlates on `(uuid, name, kind)` because of the UUID collisions; monoliths are
  rebuilt from shards on single-file re-index.
- **T10:** `src/report.rs` + `src/bin/summarize.rs` (the plan only fixed the CLI name); tables
  render into `BEGIN/END:table` blocks only.

## 16. Proposed Phases 1–7 (revised)

Consistent with [Appendix F](PLAN_PHASE0_SPIKES_0.md#appendix-f--decisions-carried-into-phases-17),
adjusted to the evidence:

1. **Phase 1 — skeleton + quality gates.** Root crate mirroring knot (config, `cli_tools/run_*`,
   `mcp_tools/*`, `mcp_handler` with `tools()`+`dispatch()`), edition 2024, knot lints verbatim,
   `make check` green from the first line. *(nothing in Phase 0 blocks this)*.
2. **Phase 2 — index + capability tiers.** `knot-rag index` = knot parse → `.knot-rag/` shards
   (`i8pv_b64` + `nov` metadata; chunk/file tier-1 records) + **pre-computed LSP edges** for
   high-fan-in targets (the G8 latency fix) + heuristic edges as tier-2 fallback. `status`
   shows the tier matrix. *Fix or work around knot's UUID issues here (hash-derived ids).*
3. **Phase 3 — retrieval (S2 semantics).** Vector search over shards (binary cache for warm
   loads), tier-1 file location, query-time provenance (`lsp-live | lsp-indexed | heuristic`).
4. **Phase 4 — graph ops (S1 semantics).** `find_callers`/`explore_file` over LSP live +
   pre-computed edges, with the ownership granularity of the gold rule; retries on
   `ContentModified`.
5. **Phase 5 — MCP layer.** `rust-mcp-sdk` 2.x (stateless 2026-07-28 revision, no `initialize`);
   warm LSP pool living in the server process keyed by repo root + language.
6. **Phase 6 — MCP/CLI parity + dry-run.** Same handler shape as knot's; diagnostics and
   degradation surfaced in both surfaces (T8 behaviour as product behaviour).
7. **Phase 7 — E2E + docs.** `tests/run_all_e2e.sh` (picked up automatically by `make e2e`),
   README, runbook for the tier matrix and the opt-in committed index.

## 17. Reproduction

See [`docs/spikes/phase0/README.md`](phase0/README.md) for the exact commands (sandbox setup,
`make check`, S1 matrix, S2 matrix, gold adjudication, report rendering). Raw artefacts:
`docs/spikes/phase0/results/` (committed JSON) and `docs/spikes/phase0/results/raw/` (gitignored
logs). Every table in this report is regenerated by:

```bash
cd docs/spikes/phase0 && cargo run --release --bin summarize -- --results results --out ../phase0-results.md
```
