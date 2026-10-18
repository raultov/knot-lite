# Changelog

All notable changes to `knot-lite` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.6.1] - unreleased

### Fixed
- **The 0.6.0 measurements were not taken and are corrected here.** R1 and P1 returned hard-coded
  results, E1 never edited a file, J1 reported `off` as 0/8 when it was 8/8, and P6-D12, L7, X2 and
  M5 were never run. Everything below is measured on work copies of `knot`, `knot-site` and
  `spring-ai` with real language servers; the raw numbers are the JSON files of
  `~/.local/share/knot-lite/measure/p61/` and are quoted in `README.md`.

### Measured
- **Gate E1 (changed-set cost):** `knot` p95 2 ms (0 changes), 3 ms (1), 2 ms (20). `spring-ai`
  p95 28 ms (0 changes), 28 ms (1), 28 ms (20) — +0 ms with 20 changes.
- **Gate R1 (re-anchoring):** 20 targets per repository with shifted lines, a new caller and one
  renamed unrelated local, plus 5 deleted declarations. `knot` 20/20 identical, `spring-ai` 20/20
  identical, both 5/5 `stale_target`; local renames hit 16/20 and 5/20. `knot-site` is partial
  (only 24 qualifying targets): 19/19 identical at 19 targets, 5/5 `stale_target`, 5/19 renames.
- **Gate P1 (freshness rule):** 10/10 targets across the four cases (indexed, unrelated edit, added
  caller, outdated fallback), p95 32–33 ms.
- **Gate J1 (Java edit sync):** four modes × three scenarios. `off` misses the new caller in 2/16
  (S1), 2/8 (S2) and 8/8 (S3) sessions; `watched` completes 32/32, `open` and `open+watched` too
  except `open` on S2 (6/8). Every mode stays within `off + SYNC_WAIT`; `watched` is the default
  again under the 5 % noise rule (`open+watched` is 0.6 % cheaper and also passes).
- **Gate P6-D12 (Rust/TS edits):** 8/8 in each of the four cases — the E2E fixtures and the
  `knot` / `knot-site` work copies with the new caller in another module — with `sync=off`.
- **Regressions:** M5 load medians `knot` 23 ms, `knot-site` 4 ms, `spring-ai` 198 ms; L7 parity
  10/10 on `spring-ai`; X2 agreement 20/20 on `knot` and `knot-site`.

### Known limitations
- **Gate E1 missed its 20 ms target on `spring-ai`** (28 ms with 0 changes, from 2,278 sequential
  `stat` calls, ~12 µs per file). Accepted under P6-D16: the walk adds +0 ms when files change and
  `knot` is unaffected. The cost grows linearly with repository size.
- **Gate R1 on `knot-site` is partial:** only 24 qualifying targets exist (one per file), so it ran
  at 19 targets and is not counted as passed (decision D4). Its results are 19/19 identical.
- **The sync cost of `watched` is real:** the first answer after an edit gets slower (S1 ~7.3 s
  against 4.5 s, S3 ~17 s against 16.4 s) because jdtls re-reads the changed files first.

## [0.6.0] - 2026-10-18

### Added
- **Re-anchor stale targets:** `find_callers` and `explore_file` re-parse changed files using knot's parser, re-anchor shifted targets to their fresh line and column (`lsp.reanchored = true`), and continue live language-server queries without waiting for `knot-lite index`.
- **Overlay caller attribution:** live language-server reference locations in changed files are grouped by fresh container tables; newly added containers not present in the index are emitted with `indexed: false`.
- **Pre-computed freshness rule (P6-D8):** `lsp-indexed` answers are served only when no edit can have affected them (target file unchanged, no pre-computed caller in a changed file, and no changed file mentions the target's name).
- **Outdated indexed fallback (P6-D9):** when an outdated target cannot be answered live by the server, it falls back to the pre-computed answer marked with `lsp.outdated = true` and an explanatory diagnostic note.
- **Language server edit synchronization (`sync`):** jdtls receives `workspace/didChangeWatchedFiles` notifications for modified and deleted project files before requests (`SyncMode::Watched`, frozen as default based on gate J1); `KNOT_LITE_LSP_SYNC` allows configuring `off`, `open`, `watched`, or `open+watched`.
- **E2E tests for edits:** rows covering stale declaration deletion (`*44`), line shifting (`*45`), unrelated edits with pre-computed reuse (`*46`), new caller detection (`*47`), server failure fallback to outdated (`*48`), and live MCP edit-awareness (`*49`).

## [0.5.0] - 2026-10-10

### Added
- **Glob path filters:** `path` of `search_hybrid_context` and `list_files` accepts knot's glob language (`*` within a segment, `**` across segments, `?`); plain paths stay directory prefixes. Responses report `path_mode` (`prefix` / `glob`).
- **`find_callers` target tiers:** knot's full ladder — exact FQN, FQN suffix, exact name, signature prefix, name prefix (short queries) and fuzzy substring (4+ characters). `resolution.tier` names the tier used; approximate tiers add a note.
- **Hybrid search (opt-in, `KNOT_LITE_SEARCH=hybrid`):** knot's recall pool built in memory (cosine window, definition pass, name probe, callers of the best roots), a BM25 keyword channel over a new per-file lexicon (`.knot-lite/lexicon/`, tokenizer `kl-lex-1`), knot's kind-aware `rerank`, reciprocal-rank fusion. `status` shows the search mode.
- **`explore_file` v2 (`knot-lite/explore/v2`):** live language-server reference counts for up to 40 entities per call (about 4 s, types and callables first; `lsp.status` = `ok` / `partial` / `off` / …); a changed or unindexed file is described by the server's `documentSymbol` outline.
- **Pre-computation selection (opt-in, `--lsp-selection lsp-refs`):** a `references` probe over a wider candidate pool (`--lsp-probe-min-fanin`, default 3) ranks targets by the server's own caller count. Manifest `[lsp] format = 2` records the selection and probe counts.
- **Continuous integration:** `make check` on Linux (and macOS on `master`), `make e2e` on Linux in strict mode with pinned language servers.
- **E2E rows** for globs, tiers, search modes, explore (servers off, live counts, outlines) and both selections.

### Changed
- The selection and search defaults stay as in 0.4.x: hybrid search and the `lsp-refs` selection missed this release's measurement bars (hybrid: +6.6 points of hit@5 on held-out queries vs +10 required; `lsp-refs`: 100 % / 70 % overlap with the most-referenced targets and ×1.05 / ×1.12 callers vs 85 % and ×1.10 required). See the README's Phase 5 measurements.
- `search_hybrid_context` answers with schema `knot-lite/search/v2` in both modes: every hit carries `cosine` and `channels`.
- `explore_file` waits for a starting language server before its first request, as `find_callers` does; its live budget starts once the server answers.
- Shard records carry `tc` (entity inside a Rust `#[cfg(test)]` module). Indexes built before 0.5.0 still load; the keyword channel reads names, FQNs and signatures only until the next `knot-lite index`.

---

## [0.4.0] - 2026-10-09

### Added
- **E2E Integration Test Suites:** Added comprehensive End-to-End integration test suites (`tests/run_*_e2e.sh`) covering Rust, TypeScript, JavaScript, Maven, and Gradle projects.
- **E2E Orchestrator:** Added `tests/run_all_e2e.sh` (`make e2e` / `make check-all`) with per-suite summaries, process leak assertions, and strict LSP requirement modes (`KNOT_LITE_E2E_REQUIRE_LSP=1`).
- **Granular LSP Pool Locking:** Replaced the global pool lock with per-language locks (`LangCell`), preventing query contention across independent language servers during parallel index snapshots.
- **Gradle Daemon Containment & Ownership:** Implemented owner tokens (`KNOT_OWNER_<PID>`) and process sweeps (`src/lsp/owner.rs`), ensuring language server processes and child Gradle daemons are strictly killed when knot-lite exits.

### Fixed
- **jdtls Readiness on Gradle Projects:** Fixed an issue where jdtls progress notifications (`Starting`) after `ServiceReady` caused readiness checks to hang on Gradle projects by making `ServiceReady` sticky once progress settles.
- **jdtls Gradle JVM Arguments:** Configured private Gradle JVM arguments (`-XX:ErrorFile=...`) so child Gradle daemons inherit the ownership marker and are cleaned up on server termination.
- **Java Re-indexing Retries in E2E:** Added retry logic for Java live query assertions in E2E tests to account for asynchronous background indexing by jdtls after file edits.

---

## [0.3.0] - 2026-10-08

### Added
- **Language Server Protocol (LSP) Integration:** Integrated async LSP client stack supporting `rust-analyzer`, `typescript-language-server`, and `jdtls`.
- **Hybrid Caller Resolution (`find_callers` v2):** Enhanced reverse-dependency lookup to query live language servers, pre-computed LSP edge shards, and heuristic graph fallbacks per target entity.
- **Pre-computed LSP Edge Shards:** Added pre-computation of LSP call locations during indexing for high fan-in targets with incremental content-hash reuse.
- **LSP Process Pool:** Added process-wide managed LSP pool with lazy initialization, thread-safe request routing, and automatic recovery on server crash.
- **LSP CLI Flags:** Added `--no-lsp` and `--no-precomputed-lsp` flags to CLI queries to control LSP live lookups and pre-computed edge usage.

### Changed
- **Knot Core Dependency:** Updated `knot` dependency to 1.11.3 for deterministic reference resolution.

---

## [0.2.0] - 2026-10-07

### Added
- **Local Embedded Indexer (`knot-lite index`):** Implemented lightweight local indexing pipeline with file selection, inventory, content hashing, and manifest v1.
- **Vector Embeddings (`search_hybrid_context`):** Added local vector database support via `fastembed` / `KnotEmbedder` with 60-line sliding window chunking and incremental content-hash vector reuse.
- **Heuristic Graph Analysis:** Ported knot's structural heuristic resolver for container tables, symbol relationships, and call graph construction without external graph databases.
- **Query Tools CLI & MCP:** Added full implementations for `search_hybrid_context`, `find_callers`, `explore_file`, `list_files`, and `status`.

---

## [0.1.0] - 2026-10-06

### Added
- **Initial Crate Skeleton:** Established lightweight Rust binary (`knot-lite`, `knot-lite-mcp`) matching knot's tool surface and clap CLI interface.
- **Transport-Independent MCP Handler:** Implemented MCP stdio transport layer (`mcp_handler`) with JSON schema validation for all query tools.
- **Capability Detection:** Added system environment scanning for available language servers (`rust-analyzer`, `typescript-language-server`, `jdtls`) and toolchain binaries.
- **Typed Diagnostics:** Added `QueryUnavailable` and `IndexUnavailable` typed diagnostic errors for graceful LLM tool error reporting.
