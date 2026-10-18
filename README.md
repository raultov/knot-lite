# knot-lite

[![CI](https://github.com/raultov/knot-lite/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/raultov/knot-lite/actions/workflows/ci.yml)

Database-free code intelligence for **one repository**: file-based vectors plus a
language-server call graph, built on [knot](https://github.com/raultov/knot)'s parsers. No Qdrant, no Neo4j,
no services to run — an index lives in a single `.knot-lite/` directory inside the
repository it describes.

> **Project status — Phase 5.** All five tools (`status`, `search_hybrid_context`, `find_callers`,
> `explore_file`, `list_files`) answer from the index. `find_callers` asks the **language
> server** (rust-analyzer, typescript-language-server, jdtls) per target — live, or from edges
> pre-computed by `knot-lite index` for widely used targets — and falls back to knot's heuristic
> edges per target when a server is unavailable. Vectors use `i8pv_b64` int8 product-quantisation
> with text-hash reuse. Phase 4 hardened the servers: languages are queried in parallel, every
> process a server starts (a Gradle daemon) is owned by it and stopped with it, and a shell
> end-to-end suite runs the real servers on small fixtures (`make e2e`). Phase 5 added glob path filters, knot's
> full target-tier ladder for `find_callers`, an opt-in hybrid search (`KNOT_LITE_SEARCH=hybrid`), language-server
> counts and outlines in `explore_file`, an opt-in pre-computation selection by server reference counts, and CI.

## Quick start

```sh
knot-lite index                               # build .knot-lite/ in the current directory
knot-lite status                              # check capability matrix, index state and freshness
knot-lite search "user authentication"        # vector search
KNOT_LITE_SEARCH=hybrid knot-lite search "user authentication"  # opt-in hybrid ranking
knot-lite callers parse_files                 # reverse references (language server, heuristic fallback)
knot-lite callers parse_files --no-lsp        # same, without asking language servers
knot-lite callers arse_fil --no-lsp           # approximate tiers: substring (4+ chars), name prefix, signature prefix
knot-lite status --probe-lsp                  # start each language server once and report readiness
knot-lite explore src/pipeline/parser/mod.rs  # file anatomy and entity list
knot-lite files -p src/                       # list indexed files
knot-lite files -p 'src/**/*.rs'              # ... or the files matching a glob
```

## Tools & Capabilities (0.6.0)

| Tool | Capability | Provenance |
|---|---|---|
| `status` | Capability matrix (tiers 3/2/1/0 per language) + index details, counts, model, freshness; language-server state (running, version, memory, sync mode, pre-computed or not) | — |
| `search_hybrid_context` | Cosine top-k over entities and tier-1 line-window chunks (default), or opt-in hybrid ranking with `KNOT_LITE_SEARCH=hybrid`: recall pool + BM25 keyword channel, knot's kind-aware re-rank, reciprocal-rank fusion | `vector` / `hybrid` |
| `find_callers` | knot's target tiers (exact FQN → FQN suffix → exact name → signature prefix → name prefix / fuzzy substring), container-level callers grouped by relationship; targets in changed files re-anchored live (`lsp.reanchored = true`) and attributed through fresh overlay containers (`indexed: false` for new containers); per-target source | `lsp-live` / `lsp-indexed` / `heuristic` |
| `explore_file` | File entities grouped by family, with distinct-caller counts per entity and markup summaries; counts on changed files attributed through fresh overlay containers; a changed or unindexed file of a language-server language is described by the server's outline | `lsp-live` / `lsp-indexed` / `heuristic` per entity |
| `list_files` | Indexed files with entity and chunk counts, prefix or glob filter | — |

## Indexing (`knot-lite index`)

`knot-lite index` builds `<repo>/.knot-lite/`:
- `manifest.toml`: index metadata, counts, model, timings, commit mode.
- `inventory.jsonl`: file path, size, and SHA-256 hash stamp for freshness detection.
- `shards/<rel>.jsonl`: int8 quantised vector records for parsed code entities and tier-1 line-window chunks.
- `lexicon/<rel>.jsonl`: the term bag of every record with a vector (tokenizer `kl-lex-1`), read by the keyword channel of `search_hybrid_context`. Shard records also carry `tc` (an entity inside a Rust `#[cfg(test)]` module).
- `edges/<rel>.jsonl`: pre-computed reference relationship edges, keyed by caller file (knot's heuristic resolver).
- `lsp-edges/<lang>/<rel>.jsonl` and `lsp-targets/<lang>.jsonl`: language-server answers pre-computed for widely used targets (see below).
- `.gitignore` (default) or `.gitattributes` (`--commit`).

Index flags:
- `--force`: ignore reusable vectors and re-embed everything.
- `--commit`: track `.knot-lite/` in git (writes `.gitattributes`). Default is `--gitignore`.
- `--embed-model <model>`: embedding model choice (`AllMiniLML6V2` default, `BGEBaseENV15` opt-in).
- `-j, --jobs <N>`: parallel parser threads.
- `--batch-size <N>`: passages per embedding call (default: 128).
- `-q, --quiet`: suppress progress output on stderr.
- `--lsp-precompute <auto|all|none>`: pre-compute language-server callers for widely used targets — `auto` = Java only (default), `all` = every language with a server, `none` = skip (and remove earlier data).
- `--lsp-min-fanin <N>` (default 20), `--lsp-max-targets <N>` (200), `--lsp-budget-secs <N>` (600): which targets, how many, and how long per language.
- `--lsp-selection <heuristic|lsp-refs>`: how targets are chosen. `heuristic` (default) uses the heuristic caller count only (the 0.4.x rule). `lsp-refs` (opt-in) first asks the server for the references of a wider candidate pool (`--lsp-probe-min-fanin <N>`, default 3 heuristic callers, plus every interface, class and type; at most 4 × `--lsp-max-targets` candidates, within 40 % of the budget) and pre-computes the candidates with the most callers according to the server (≥ `--lsp-min-fanin`), reusing the probe's references in the answer; candidates the probe did not reach fall back to the heuristic rule. It picked better targets than `heuristic` in the Phase 5 measurements (overlap with the most-referenced targets 100 % vs 86 % on knot, 70 % vs 55 % on a spring-ai sample) but missed this release's bars (85 % overlap and +10 % callers covered), and its probe doubles the language-server time on knot, so it is not the default. `status --output json` shows the selection and how many candidates were probed per language.

A warm re-index whose language files did not change reuses the pre-computed data and **starts no server**.

## Capability tiers

Every language row in `status` reports one of these tiers (Phase 0 §2):

| Tier | Meaning | What knot-lite can answer |
|---|---|---|
| **3 — Full** | knot parser + language server found | entities + vectors; language-server graph (`lsp-live`, or `lsp-indexed` when pre-computed), heuristic fallback per target |
| **2 — No LSP** | parser exists, language server missing | entities + vectors; **heuristic** graph |
| **1 — Files only** | no parser (`.sh`, `Makefile`, `Dockerfile`, …) | line-window chunks and file-level vectors |
| **0 — Excluded** | binaries, huge files (> 256 KiB), lock files | nothing (gitignored files are never counted) |

## Language servers

`find_callers` answers each target from the first source that applies: **`lsp-indexed`** (the target was
pre-computed by `knot-lite index`; pre-computed data is read even when servers are off), **`lsp-live`**
(the server is asked now) or **`heuristic`** (knot's edges). Every target states its source and, when a
server was considered but did not answer, why (`not_ready`, `no_server`, `failed`, `stale_target`,
`budget`, `deadline`, `no_column`, `unsupported_kind`, `disabled`, …).

- **Per language.** Rust → `rust-analyzer` (`rustup component add rust-analyzer`); TypeScript/JavaScript →
  `typescript-language-server` (`npm install -g typescript-language-server typescript`; the project's own
  `node_modules/typescript` must be resolvable); Java → jdtls 1.61+ on `PATH`. `KNOT_LITE_LSP_RUST/_TS/_JAVA`
  override the program.
- **When they start.** Lazily, on the first target of their language (CLI and MCP), unless
  `KNOT_LITE_LSP_WARMUP=on` (MCP: start the indexed languages at launch). A query waits for a starting server
  up to its ready timeout on the CLI and 20 s on MCP; after that the target is answered from heuristic edges
  and the note says so — retry for language-server answers. `explore_file` waits the same way before its first
  request; its own live budget (about 4 s) starts once the server answers.
- **When a server counts as ready.** jdtls announces `ServiceReady` and then keeps sending `Starting` status
  reports for its initialization jobs; a Gradle import goes on after `ServiceReady`. A Java server is ready once it
  has announced `ServiceReady` **at any time** and those reports have stopped for the idle window (it is not the
  last report that counts). Before 0.4.0 the last report was compared, so a Gradle project never became ready and
  always fell back to heuristic edges. Measured on a one-class Gradle project: `lsp-live` in 7–13 s with a warm
  jdtls workspace (one run, right after the project was first imported, took 43 s).
- **When they stop.** The CLI stops them when it exits. The MCP server stops a server unused for
  `KNOT_LITE_LSP_IDLE_SECS` (default 900) and restarts it on demand; a server that dies is restarted after a
  back-off (5 s, then 30 s, 120 s) and disabled after three failures. Stopping a server also stops the helper
  processes it started on our behalf (a Gradle daemon): they carry the server's ownership mark and are swept with
  it (see Security).
- **Where they write.** Only under `<repo>/.knot-lite/` (the index) and the per-repository scratch directory
  `$KNOT_LITE_LSP_CACHE_DIR/<repo key>/` (rust-analyzer's target dir, jdtls's workspace, the last 5 stderr logs
  per language). Measured: the repository is left byte-identical (see Security for the one thing this relies on).
- **Budget per call.** At most `KNOT_LITE_LSP_MAX_LIVE_TARGETS` (default 10) live targets and
  `KNOT_LITE_LSP_DEADLINE_SECS` (default 30 s) of live time per `find_callers`; the rest fall back, with a note.
- **Memory** (resident, measured): rust-analyzer ≈ 3.1 GiB on a 4.8 k-entity repository, jdtls ≈ 1.4 GiB on
  `spring-ai` (it may grow up to its `-Xmx8g`), typescript-language-server ≈ 0.6 GiB on a 1.5 k-entity repository.
  `status` shows each running server's pid, readiness time and memory.
- **Edit awareness (0.6.0).** Language servers no longer fall back to heuristic when a file is edited. On each query,
  knot-lite detects modified or deleted files (`EditedFiles`) using coarse mtime and size filters confirmed by SHA-256 checks:
  - *Re-anchoring:* If a target's declaration moved or its file changed, the file is re-parsed with knot's parser,
    the declaration is matched and repositioned (`lsp.reanchored = true`), and the request is sent to the server at its fresh position.
  - *Overlay attribution:* Server response locations in changed files are mapped to fresh containers; newly added methods/functions
    unknown to the index are emitted with `indexed: false`.
  - *Freshness rule for pre-computed answers:* Pre-computed targets are served `lsp-indexed` only when their own file is unchanged,
    no pre-computed caller is in a changed file, and no changed file of that language mentions the target's name as an identifier.
    Otherwise, knot-lite asks the language server live and, if the live query fails, falls back to the pre-computed answer marked with
    `lsp.outdated = true`.
  - *Edit synchronization:* For language servers that need workspace change notifications (jdtls), knot-lite synchronizes changed files
    before each query via `workspace/didChangeWatchedFiles` (`SyncMode::Watched`, configured by `KNOT_LITE_LSP_SYNC`), ensuring the server's
    in-memory index stays fresh.

## Security

Starting a language server on a repository **runs that repository's build code**, as opening it in an editor
does: rust-analyzer runs build scripts and proc-macros (`cargo check` into the scratch directory), jdtls
imports Maven/Gradle models, and tsserver loads the project's TypeScript. Only run knot-lite on repositories
you would open in an editor. Set `KNOT_LITE_LSP_BUILD_EXEC=off` to disable rust-analyzer build scripts and
proc-macros and jdtls's Gradle import (accuracy drops where code is generated), or `KNOT_LITE_LSP=off` to never
start a server (pre-computed results are still read). jdtls is started with its auto-build off so that it does
not compile into `target/` of your modules, and with Eclipse metadata files disabled; on a Maven repository
(`spring-ai`) the working tree was measured byte-identical before and after (every file; directories are not
compared). On a one-module Maven project jdtls does create its two empty output directories, `target/classes` and
`target/test-classes` — no file is written there, and Maven repositories ignore `target/`. **Gradle projects while build execution is on**:
jdtls runs Gradle on them, which writes a `.gradle/` project cache into the project and may download a Gradle
distribution into `~/.gradle` (as any IDE does). The Gradle daemon it starts is **private to that jdtls session**
(a unique JVM argument of ours makes it incompatible with your own builds, so one never attaches to it) and is
**stopped with the server** — on idle, on re-index, on exit. If knot-lite is killed, the first start of the next
knot-lite removes it. Everything a language server starts on our behalf carries an ownership mark (an environment
entry, and where it survives on the command line `-Dknot.lite.lsp.owner=<token>`, plus
`-XX:ErrorFile=<scratch>/gradle-owner-<token>.log` for the Gradle daemon, whose `-D` options never reach its
command line): only processes carrying that exact mark are ever signalled, never a process by name. Use
`KNOT_LITE_LSP_BUILD_EXEC=off` on repositories with Gradle projects you do not want touched. The
servers' own global caches (`~/.m2`, `~/.cargo/registry`, jdtls's install-local configuration) are written as by
any editor.

## Performance & Baselines (Phase 2 Measurements)

- **Cold Index Time:** ~150 s for `knot` (~5.8k entities), ~24 s for `knot-site` (~1.5k entities), ~12 min for `spring-ai` (~33.5k entities).
- **Warm Re-index Time:** ~0.6 s for `knot`, ~0.2 s for `knot-site` (0 vectors re-embedded).
- **Single Function Append:** ~0.6 s for `knot` (1 vector re-embedded, wall time << 10% of cold run).
- **Index Load Median Latency:** 16 ms for `knot`, 3 ms for `knot-site`, 163 ms for `spring-ai`.
- **Retrieval Accuracy (Entity Domain hit@5):** `knot` 75.0 %, `knot-site` 80.0 %, `spring-ai` 83.3 %.

## Performance & Baselines (Phase 3 Measurements)

Measured with the release build on the same three repositories (see `docs/PLAN_PHASE3_LSP.md`, Appendix D):

- **Live `find_callers`, warm:** p95 46 ms (Rust, `knot`) and 34 ms (TypeScript, `knot-site`). Java targets that
  are not pre-computed take a median of 1.7 s and p95 16 s per target.
- **Pre-computed (`lsp-indexed`):** p95 13 ms per target on `spring-ai`; the default `auto` policy computed 176 of
  178 selected Java targets (18 277 edges) inside its 600 s budget.
- **First answer after starting the MCP server (Rust, `knot`):** 7.6 s, from `lsp-live`.
- **Warm re-index (nothing changed):** 0.9 s `knot`, 0.4 s `knot-site`, 9 s `spring-ai`; no server is started.
- **Agreement with an independent client** (the Phase 0 probe re-run on the same snapshot): 20/20 Rust and 21/21
  TypeScript targets identical; Java 99.0 % micro recall, the differences being comment-only references that are
  dropped on purpose.
- **Deterministic indexes (knot 1.11.3):** two cold indexes of `knot` and of `knot-site` are byte-identical apart
  from the manifest timestamps, and the heuristic edges of three consecutive `spring-ai` re-indexes are
  byte-identical. knot 1.11.2 resolved some references by the first hit of a hash-map iteration, so the same sources
  could index differently; 1.11.3 fixes that and is the minimum version. With the language servers switched off
  (`KNOT_LITE_LSP=off`) the Phase 2 numbers hold: hit@5 75.0 / 80.0 / 83.3 %, load median 17 ms (`knot`), 3 ms
  (`knot-site`), 176 ms (`spring-ai`).

## Performance & Baselines (Phase 4 Measurements)

Measured with the release build of 0.4.0 (knot 1.11.3) on the same repositories, plus a polyglot repository
(`spring-ai` and a Rust crate) and a one-class Gradle project (see `docs/PLAN_PHASE4_HARDENING_E2E.md`, Appendix D):

- **Pre-computed (`lsp-indexed`), re-measured:** p95 13 ms per target and 10/10 targets identical to the live answer.
  The default `auto` policy computed 141 of 144 Java targets (17 850 edges) inside its 600 s budget on an otherwise
  idle machine (Phase 3: 176 of 178, 18 277 edges — the budget is wall-clock, so how far the ranked list gets
  depends on the machine). The selection is reproducible: with a 1 500 s budget two independent runs selected the
  same 200 targets (197 answered, 20 956 edges both times), and the 144 of the 600 s run are a prefix of them.
  Warm re-index: 8.9 s, no server started.
- **Parallel languages:** while a Java target took 16 s live, five Rust queries on the same repository answered in
  40–75 ms each, and `status` took at most 42 ms (63 samples) with the Java server `ready`.
- **Gradle:** a Gradle project answers `lsp-live` (7–13 s warm). In every scenario the Gradle
  daemon carried the ownership mark while the server ran and none was left afterwards: 10 s after the CLI exited,
  3.6 s after the MCP idle reaper stopped Java, immediately after the MCP server exited, and — after `kill -9` of the
  MCP server, which leaves jdtls and the daemon running — at once on the next knot-lite start.
- **Live latency after the pool rewrite:** unchanged — p95 46 ms (Rust, `knot`) and 28 ms (TypeScript, `knot-site`),
  20/20 and 21/21 targets identical to the independent client.

## Performance & Baselines (Phase 5 Measurements)

Measured with the release build of 0.5.0 (knot 1.11.3) on the same three repositories; the baselines are 0.4.0
re-measured the same day (see `docs/PLAN_PHASE5_SEARCH_LSP_CI.md`, Appendix D):

- **Hybrid search (opt-in):** on a held-out set of 30 hand-written queries, hit@5 83.3 % hybrid vs 76.7 % cosine
  (+6.6 points; the bar for making it the default was +10, so `vector` stays the default). Per repository (cosine /
  hybrid): `knot` 6/8, `knot-site` 9/8, `spring-ai` 8/9. On the Phase 2 entity queries hybrid reaches 75.0 / 100 /
  100 % (cosine: 75.0 / 80.0 / 83.3 %). In-process search p95: 2.3 ms (`knot`), 0.8 ms (`knot-site`), 12.8 ms
  (`spring-ai`); the keyword index is built on the first hybrid query (34 / 6 / 224 ms).
- **Index cost of the lexicon:** warm re-index ×1.06 / ×1.08 / ×1.04 of 0.4.0 (803 ms, 398 ms, 10.2 s); `.knot-lite`
  grows by 66 / 51 / 53 % (lexicon 3.2 / 0.6 / 17.6 MB). Index load median 19 / 3 / 192 ms.
- **`explore_file` with a warm server:** p95 84 ms (TypeScript, `knot-site`); 1.8 s (Rust, `knot`), the slow sample being
  the first explore of a large file (the 14 others: 7–641 ms). Java on `spring-ai` hits the 4 s live budget and answers
  `partial`. Live counts agree with `find_callers` on 20/20 Rust and 20/20 TypeScript entities.
- **Pre-computation selection** (`--lsp-precompute all` on `knot`): `heuristic` picked 48 targets (2 942 edges, 13.7 s of
  language-server time); `lsp-refs` probed 536 candidates in 14.4 s and picked 50 (3 103 edges, 29.1 s). Against the
  server's own most-referenced targets, `lsp-refs` overlapped 100 % (heuristic 86 %) and covered ×1.05 the callers; on a
  20-target `spring-ai` sample 70 % vs 55 % (the probe reached 12 of 80 candidates within its share of the budget), ×1.12.
  The bars for making it the default were 85 % overlap, +10 points and ×1.10, so `heuristic` stays the default.
  Answers pre-computed with `lsp-refs`: p95 2 ms, 10/10 identical to the live answer.

## Performance & Baselines (Phase 6 Measurements)

Measured with the release build of 0.6.1 (knot 1.11.3) on work copies of `knot`, `knot-site`, and `spring-ai` (see `docs/PLAN_PHASE6_1_MEASUREMENTS.md`, outputs in `~/.local/share/knot-lite/measure/p61/`):

- **Changed-set detection (Gate E1, `e1_changed_set_cost.json`):**
  - `knot`: p95 latency 2 ms (0 changes, bound 3 ms), 3 ms (1 change), 2 ms (20 changes, bound 13 ms) -> PASS.
  - `spring-ai`: p95 latency 28 ms (0 changes, bound 20 ms), 28 ms (1 change, bound 30 ms), 28 ms (20 changes, bound 30 ms). The 0-change bound was exceeded by 8 ms due to 2,278 sequential `stat` calls (~12 µs/file); scaling with 20 changes added +0 ms (well within the +10 ms gate).
- **Re-anchoring accuracy (Gate R1, `r1_reanchoring.json`):**
  - `knot`: 20/20 targets identical between live re-anchoring and full re-index; 5/5 deleted declarations diagnosed as `stale_target` -> PASS.
  - `knot-site`: 18/18 targets identical; 5/5 deleted declarations diagnosed as `stale_target` -> PASS.
  - `spring-ai`: 20/20 targets identical; 5/5 deleted declarations diagnosed as `stale_target` -> PASS.
- **Pre-computed freshness rule (Gate P1, `p1_freshness.json`):**
  - 10/10 Java targets passed all 4 cases: (a) unchanged target served `lsp-indexed` (p95 32 ms), (b) unrelated edit served `lsp-indexed` (p95 33 ms), (c) target with added caller served `lsp-live` with outdated note (10/10), (c′) server failure on outdated target fell back to `lsp-indexed` with `outdated: true` (10/10) -> PASS.
- **Java edit synchronization (Gate J1, `j1_sync.json`):**
  - Without sync (`off`), jdtls served stale answers in 4 of 16 sessions (2/8 in CLI re-index scenario S1, 2/8 in long session scenario S2).
  - With `watched` (`workspace/didChangeWatchedFiles`), 16/16 sessions completed with fresh answers (8/8 S1 at 7.3 s avg, 8/8 S2 at 113 ms avg).
  - `open` mode also completed 16/16, but with higher latency (404 ms avg in S2). `watched` is confirmed as the optimal default.
- **Rust and TypeScript in long sessions (P6-D12, `rust_ts_edits.json`):**
  - 8/8 Rust and 8/8 TypeScript edit iterations completed cleanly with `sync=off` in long-lived MCP sessions, confirming that rust-analyzer and tsserver built-in file watching detects edits without explicit client synchronization.
- **Regressions (0.6.1 vs Phase 5):**
  - M5 load median (`m5_load.json`): `knot` 23 ms (baseline 20 ms), `knot-site` 4 ms (baseline 4 ms), `spring-ai` 198 ms (baseline 222 ms).
  - L7 pre-compute parity (`l7_precompute.json`): 10/10 targets identical (100.0%) on `spring-ai`.
  - X2 explore agreement (`x2_explore_agreement.json`): 20/20 agree on `knot`, 20/20 agree on `knot-site`.

## Limitations

- **Hybrid search is opt-in:** by default `search_hybrid_context` ranks by cosine similarity (as in 0.4.x). `KNOT_LITE_SEARCH=hybrid` turns on knot's candidate pool built in memory (cosine window, definition pass, name probe, callers of the best roots) plus a BM25 channel over the indexed text, ordered by knot's own re-rank (`rerank`, called from the knot crate) and fused with the keyword order. On the Phase 5 query set the hybrid ranking gained 6.6 points of hit@5 over cosine on the held-out queries (83.3 % vs 76.7 %) — short of the 10-point bar this release required to make it the default — and lost one `knot-site` query; it is stronger on identifier queries and weaker on documentation/config questions, where knot's re-rank prefers definitions. The call-graph signal uses the heuristic `CALLS` edges only. Indexes built before 0.5.0 have no lexicon: the keyword channel then reads names, FQNs and signatures only until the next `knot-lite index`.
- **Heuristic fallback:** for languages without a server, and whenever a server is unavailable, still starting, disabled or hits budget/deadline limits, `find_callers` uses knot's heuristic edges (~93 % precision, ~86 % recall on Rust; indirect or dynamic calls may be missed). If a target's declaration was deleted from a changed file, it is diagnosed as `stale_target`. Edits in existing files are automatically re-anchored live and synchronized to the server; unsaved in-editor buffers are not seen until saved to disk.
- **Language servers are per call:** only the first `KNOT_LITE_LSP_MAX_LIVE_TARGETS` targets of a `find_callers` call are asked; `explore_file` asks at most 40 entities (references only, about 4 s, types and callables first) and the rest keep their pre-computed or heuristic count (`lsp.status = partial`); `search_hybrid_context` does not use servers. Pre-computed answers do not report self-references. On a changed file, live counts are attributed through fresh overlay containers.
- **Java right after an edit (resolved in 0.6.0):** jdtls is synchronized before each query via `workspace/didChangeWatchedFiles` (`KNOT_LITE_LSP_SYNC=watched`). In Gate J1 measurements, `off` produced stale answers in 4 of 16 sessions, while `watched` eliminated the lag completely (16/16 complete); set `KNOT_LITE_LSP_SYNC=off` to opt out.
- **Files created after the index:** newly created files not yet present in the index inventory are not walked during queries; run `knot-lite index` to index new files.
- **References that never spell the target's name:** the freshness rule checks whether changed files contain the target's identifier name. References that never spell the name (such as anonymous lambdas implementing a single-method functional interface) may not trigger live re-querying until re-indexed.
- **One target at a time per language:** queries of the same language are answered one after another; different languages run in parallel, and `status` never waits for a query.
- **Test code in the re-rank:** knot marks entities inside Rust `#[cfg(test)]` modules as test context; for Java,
  TypeScript and JavaScript the hybrid re-rank recognises tests by their path only.
- **`explore_file` latency:** the first explore of a large file can take more than a second with a warm server (live
  reference counts for up to 40 entities); Java usually reaches the 4 s budget and answers `partial`.
- **Path filters:** `path` accepts a repo-relative directory prefix (`src/api`, matched on a path boundary) or knot's glob language: `*` stays inside one path segment (`*.rs` matches root files only; use `**/*.rs`), `**` spans any number of segments, `?` is one character. Matching is case-sensitive; `[`, `]`, `{` and `}` are literal characters (so `app/[id]` works as a prefix), not character classes or alternations.

## Install

```sh
make install          # builds release binaries and copies them to ~/.cargo/bin
```

Produces `knot-lite` (CLI) and `knot-lite-mcp` (stdio MCP server). Override the
destination with `make install DESTDIR=/some/dir`.

## Configuration

Resolution precedence: CLI flags → process environment → `~/.config/knot-lite/.env`
(or `$KNOT_LITE_CONFIG_DIR/.env`) → defaults. The process environment is never
modified and the `.env` file is never written.

| Variable | Meaning |
|---|---|
| `KNOT_LITE_REPO_PATH` | repository root (default: current working directory) |
| `KNOT_LITE_REPO_NAME` | logical repository name (default: last path component) |
| `KNOT_LITE_EMBED_MODEL` | embedding model name (`AllMiniLML6V2` default, fallback: `KNOT_EMBED_MODEL`) |
| `KNOT_LITE_FASTEMBED_CACHE_DIR` | fastembed model cache directory (fallback: `KNOT_FASTEMBED_CACHE_DIR`, then `$HOME/.cache/knot-lite/fastembed`) |
| `KNOT_LITE_CONFIG_DIR` | directory holding the `.env` file |
| `KNOT_LITE_SEARCH` | `vector` (default, the 0.4.x cosine top-k) / `hybrid` (opt-in hybrid ranking): ranking of `search_hybrid_context`; `status` shows the mode in force |
| `KNOT_LITE_LSP` | `on` (default) / `off`: master switch for starting language servers (pre-computed data is still read when `off`) |
| `KNOT_LITE_LSP_BUILD_EXEC` | `on` (default) / `off`: no rust-analyzer build scripts or proc-macros, no jdtls Gradle import |
| `KNOT_LITE_LSP_SYNC` | `watched` (default) / `open` / `off` / `open+watched`: edit synchronization mode for language servers (jdtls) |
| `KNOT_LITE_LSP_CACHE_DIR` | root of the per-repository scratch directories (default `$HOME/.cache/knot-lite/lsp`) |
| `KNOT_LITE_LSP_READY_WAIT_SECS` | how long a query waits for a starting server (CLI: the server's ready timeout; MCP: 20) |
| `KNOT_LITE_LSP_IDLE_SECS` | MCP: stop a server unused for this long (default 900; `0` = never) |
| `KNOT_LITE_LSP_MAX_LIVE_TARGETS` | live language-server targets per `find_callers` call (default 10, cap 100) |
| `KNOT_LITE_LSP_DEADLINE_SECS` | live language-server time per `find_callers` call (default 30, cap 600) |
| `KNOT_LITE_LSP_WARMUP` | MCP: `on` starts the servers of the indexed languages at startup (default `off`) |
| `KNOT_LITE_LSP_RUST` | override for `rust-analyzer` (program name or path) |
| `KNOT_LITE_LSP_TS` | override for `typescript-language-server` |
| `KNOT_LITE_LSP_JAVA` | override for `jdtls` |
| `KNOT_LITE_DRY_RUN` | MCP only: answer protocol requests without a repository |
| `RUST_LOG` | log level (CLI defaults to `error`, MCP to `info`; logs go to stderr) |

## Troubleshooting

- **Incomplete index:** If an index run is interrupted, `status` reports `index: incomplete`. Run `knot-lite index` to complete it.
- **Model download:** First index run downloads ONNX model weights (~23 MB for `AllMiniLML6V2`). Reuse an existing cache by setting `KNOT_FASTEMBED_CACHE_DIR=~/.knot/repos/fastembed_cache`.
- **Corrupt index / model switch:** Run `knot-lite index --force` to rebuild shards from scratch.
- **A target answers `heuristic`:** read its `lsp.status` and the notes. `no_server` — the binary is missing (`status` lists it with an install hint) or the override is wrong; `failed` — the server exited (the note carries its message; for TypeScript, `typescript` must be installed in the project); `not_ready` — it is still starting, retry; `stale_target` — run `knot-lite index`. `knot-lite status --probe-lsp` starts each server once and tells you how long it took.

## MCP Server

`knot-lite-mcp` speaks **MCP 2026-07-28** (stateless) over stdio and serves exactly one
repository per instance. Example for opencode 2 (`opencode.jsonc`):

```jsonc
{
  "mcp": {
    "knot-lite": {
      "type": "local",
      "command": ["knot-lite-mcp", "--repo-path", "/abs/path/to/repo"],
      "enabled": true
    }
  }
}
```

## Quality gates

```sh
make check                                  # fmt-check, clippy, every cargo test target, dupes
make test-lib                               # fast loop: library unit tests only (not a gate)
make check CRATE_DIR=docs/spikes/phase0     # the Phase 0 spike crate's own gate
make e2e                                    # the end-to-end suites (real language servers, see below)
make check-all                              # make check + make e2e
```

The measurement gates (M1/M4/M5/M6, the Phase 3–4 L gates and the Phase 5 H1/M4r/H2/M5r/H3, X1/X2, L13/L7s,
`#[ignore]`d in `src/measure/`) run explicitly and require persistent directories — there is no temp-dir default, so results are never lost. The
L gates start **real language servers** on the sandbox copies and also need `KNOT_LITE_LSP_CACHE_DIR`:

```sh
KNOT_LITE_SANDBOX=~/.local/share/knot-lite/sandbox \
KNOT_LITE_MEASURE_OUT=~/.local/share/knot-lite/measure \
cargo test --release --lib measure:: -- --ignored --nocapture --test-threads=1
```

## Continuous integration

`.github/workflows/ci.yml` runs on pushes to `master` and `phase-*`, on pull requests to `master` and on demand:

- **`make check`** on `ubuntu-24.04`, and also on `macos-15` for `master` (other branches check on Linux only).
- **`make e2e`** on `ubuntu-24.04` after `make check` passes, in strict mode (`KNOT_LITE_E2E_REQUIRE_LSP=1`,
  `KNOT_LITE_E2E_REQUIRE_GRADLE=1`: a missing server fails instead of skipping). It installs pinned servers —
  rust-analyzer from the toolchain, `typescript-language-server` 5.3.0 with TypeScript 5.9.3, jdtls 1.61.0 (checksum
  verified, `tests/ci/install_jdtls.sh`) — and seeds Gradle 8.9 (`tests/ci/seed_gradle.sh`). On failure the answers
  and the language-server logs are uploaded as the `e2e-logs` artifact.

Actions are pinned by commit SHA. Windows is not covered by CI.

## End-to-end tests

`make e2e` runs `tests/run_all_e2e.sh`: four suites — `tests/run_{rust,typescript,javascript,java}_e2e.sh` — each on a
small fixture (`tests/e2e/fixtures/`) that is **copied** to `$KNOT_LITE_E2E_HOME/work/<suite>/` and indexed there.
Every suite drives the release binaries through the CLI and through an MCP stdio session, with the real language
server: `index`, `status`, `files` and `search` (prefix and glob filters, vector and hybrid ranking), `explore`
(servers off, live counts, the server's outline of a changed or unindexed file), `callers` (every target tier,
heuristic, live, pre-computed — with both selections in the Java suite — stale target, unresolvable server), the MCP tools and their
error paths. The Java suite also runs a Gradle project and proves that
the Gradle daemon is stopped after a CLI exit, after the MCP idle stop, after the MCP exit, and — after `kill -9` of
the MCP server — by the next start. A suite ends with a check that no process was left behind. They are not part of
`make check` (they start real servers and take about 6–7 minutes with warm caches — Java dominates — and longer the first time); a suite also runs alone,
e.g. `./tests/run_rust_e2e.sh`.

Needs: bash (3.2+), `jq`, a release build (made unless `KNOT_LITE_E2E_SKIP_BUILD=1`), and the servers —
`rust-analyzer`, `typescript-language-server` with a `typescript` package that ships `lib/tsserver.js`, `jdtls` and a
cached Gradle 8.9 (`~/.gradle/wrapper/dists/gradle-8.9-bin`). Linux and macOS; no network. A missing server skips only
the language-server rows of its suite (`SKIP` with the reason); `KNOT_LITE_E2E_REQUIRE_LSP=1` and
`KNOT_LITE_E2E_REQUIRE_GRADLE=1` turn skips into failures.

| Variable | Default | Meaning |
|---|---|---|
| `KNOT_LITE_E2E_HOME` | `~/.local/share/knot-lite/e2e` | work copies, outputs (`run/<suite>/`), language-server cache, empty config |
| `KNOT_LITE_E2E_SKIP_BUILD` | `0` | `1`: use the existing `target/release/knot-lite{,-mcp}` |
| `KNOT_LITE_E2E_REQUIRE_LSP` | `0` | `1`: a missing server or TypeScript package fails instead of skipping |
| `KNOT_LITE_E2E_REQUIRE_GRADLE` | `0` | `1`: a missing cached Gradle 8.9 fails the Gradle rows |
| `KNOT_LITE_E2E_TYPESCRIPT` | global install | a `typescript` package directory to link into the TS/JS work copies; the global TypeScript 7 has no `lib/tsserver.js` and `typescript-language-server` cannot start with it |
| `KNOT_LITE_E2E_KEEP` | `1` | `0`: delete `work/<suite>` after a green suite |
| `KNOT_LITE_E2E_LSP_TIMEOUT` | `300` | seconds the MCP checks poll for an `lsp-live` answer |

Each check prints one line (`PASS` / `FAIL` / `SKIP`), a failure adds the expected and the actual value and the file
under `$KNOT_LITE_E2E_HOME/run/<suite>/` that holds the answer, and a suite ends with
`-- <suite>: N passed, N failed, N skipped (N s)`. The exit status is 0 only when nothing failed.

## Plans & Conventions

Spike results live in [`docs/spikes/phase0-results.md`](docs/spikes/phase0-results.md);
conventions for agents are in [`AGENTS.md`](AGENTS.md).
