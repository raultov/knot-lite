# Changelog

All notable changes to `knot-lite` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.7.8] - 2026-11-27

### Fixed
- **Every target file of a multi-target `find_callers` counts as changed from the first target on** ([#22](https://github.com/raultov/knot-lite/issues/22)). A target file edited under its old mtime joined the call's changed set only when its own target was described, so the live callers of the targets listed before it were mapped to that file's stale indexed line ranges (`callers _area`: `total_area` listed `summary` instead of `scene_area`, `pair_area` gained `scene_area`). `callers_response` now runs `EditedFiles::examine_targets` over the files of every shown target before describing the first one, so the answer no longer depends on the target order.
- **`index` no longer reads in-repository symlinks to gitignored files or into `.git/`** ([#30](https://github.com/raultov/knot-lite/issues/30)). `select_files` kept every parse candidate whose canonical target was inside the root and checked reserved directories and ignore rules on the link's own path only, so a tracked `notes.md -> .env` (gitignored) or `gitcfg.md -> .git/config` was parsed: the secret's words landed in `.knot-lite/lexicon/` (committed with `--commit`) and were searchable. A file symlink is now selected only when its target is a regular file the walk reaches by its own path (not gitignored, not under `.git/`, `.knot-lite/` or a build-output directory); `explore_file` applies the same rule before outlining an unindexed symlink.
- **`explore_file` describes a new file at the requested path, not an indexed file ending with it** ([#29](https://github.com/raultov/knot-lite/issues/29)). `resolve_file` only looked at the index, so after creating `report.rs` at the repository root, `explore report.rs` answered `suffix` with the entities of the indexed `src/report.rs`. A path that names a regular file inside the repository is no longer matched as a suffix (or as ambiguous): it is described as itself — the server's outline (`not_indexed`) when one is available, else `not_found` with the "not in the index" note.
- **`explore_file` resolves an absolute path through a symlinked repository root** ([#24](https://github.com/raultov/knot-lite/issues/24)). The configured root is canonicalised, and `normalise_input_path` only stripped that canonical prefix, so with the repository reached through a symlink (`--repo-path $L`, a symlinked workspace, macOS `/tmp`) the path `$L/src/report.rs` stayed absolute and answered `not_found`. An absolute path is now also made relative to its deepest ancestor that resolves to the repository root, and empty and `.` segments are dropped anywhere (`$A//src/./report.rs`), not only a leading `./`.
- **Re-indexing with `KNOT_LITE_LSP=off` keeps the pre-computed language-server data** ([#28](https://github.com/raultov/knot-lite/issues/28)). `index` turned servers off into `--lsp-precompute none`, which removes the earlier data, so `lsp-edges/` and `lsp-targets/` were deleted and `find_callers` fell back to `heuristic`, although the README says pre-computed results are still read with servers off. The requested policy now runs without a pool: a language whose files did not change keeps its data (`reused`), a changed one is `skipped` (its old data is removed, as it no longer matches the files); only `--lsp-precompute none` removes everything. The warning says so instead of "policy ignored".
- **A missing or incomplete lexicon falls back to names** ([#25](https://github.com/raultov/knot-lite/issues/25)). With `.knot-lite/lexicon/` deleted while the manifest still declared `[lexicon]`, hybrid search built an empty BM25 index from the empty listing and reported `ranking.lexicon: full` with no keyword scores at all. `stored_lexicon` now also requires the lexicon files to hold as many records as the manifest counts; a missing directory, a missing file or a short count uses the names-only keyword channel, with its note.
- **MCP tools enforce the string lengths of their input schemas** ([#26](https://github.com/raultov/knot-lite/issues/26), [#27](https://github.com/raultov/knot-lite/issues/27)). `tools/list` publishes `minLength` / `maxLength` for `entity_name` (1–255), `query` (1–500), `kinds`, `path`, `file_path` and `repo_name`, but nothing checked them: an empty `entity_name` was answered "no match" and a 501-character `query` was searched. `parse_args` now checks every string argument against the bounds its tool's schema declares (in characters, as JSON Schema counts) and answers a violation with a tool error (`isError: true`, `invalid arguments for <tool>: '<field>' must be …`). Numeric bounds are unchanged (the runners clamp them); a `repo_name` array is still accepted.
- **`knot-lite-mcp` answers a request line over 16 MiB** ([#23](https://github.com/raultov/knot-lite/issues/23)). The stdio transport discarded a line longer than its size cap (16 MiB) with a log line on stderr and wrote nothing, so a client waiting for that request waited for ever. It now replies with the JSON-RPC invalid-request error (`-32600`, `id: null`, the line is never read so its id is unknown) and keeps serving, as it does for non-JSON lines.
- **`KNOT_LITE_DRY_RUN` takes the usual boolean values** ([#31](https://github.com/raultov/knot-lite/issues/31)). `knot-lite-mcp` accepted only `true`/`false`, so `KNOT_LITE_DRY_RUN=1` or `=on` made it exit at startup with a parse error (code 2). It now takes `on`/`off`, `true`/`false`, `yes`/`no` and `1`/`0` (case-insensitive), as the other switches do; the README states the values.

## [0.7.7] - 2026-11-21

### Fixed
- **A long-lived server re-reads a target file edited under its old mtime** ([#19](https://github.com/raultov/knot-lite/issues/19)). `LspSession::did_open` re-opened a document only when the file's mtime differed from the one recorded at the first open, so after a same-size edit whose mtime was restored (`cp -p`, `rsync -t`, `tar x`) a `knot-lite-mcp` server kept rust-analyzer on the old buffer: `find_callers` answered `lsp-live` / `ok` with no callers after asking at the re-anchored line, and `explore_file` outlined the old positions. The session now records the SHA-256 of the text it sent with the mtime and re-opens (`didClose` + `didOpen`) when either differs.
- **Live callers in a target file edited under its old mtime use the fresh containers** ([#20](https://github.com/raultov/knot-lite/issues/20)). Since #18 `EditedFiles::is_target_changed` saw such an edit and the target was re-anchored, but `build_lookup` and `LiveRequest::changed_files` still used the coarse changed set, which left the file out: the server's reference locations were mapped to the indexed line ranges of the old layout, so the target was listed as its own caller and a real caller was missed. A target file `is_target_changed` finds changed now joins the call's changed set (`is_changed`, `changed`, `all_changed`, the new `has_changes`), and a re-anchored target's file is always checked first, so its locations are mapped through the fresh parse and the request lists it for edit sync.
- **`.edits.changed_files` counts a target file edited under its old mtime** ([#21](https://github.com/raultov/knot-lite/issues/21)). `find_callers` and `explore_file` filled `.edits.changed_files` from the coarse pass only, so it read `0` next to `reparsed: 1` while `status` reported the file changed. `EditStats::changed` (and `hashed`) now include the target files `is_target_changed` examined.

## [0.7.6] - 2026-11-19

### Fixed
- **A same-size edit with an old mtime is seen in the target's own file** ([#18](https://github.com/raultov/knot-lite/issues/18)). `EditedFiles` hashes a file only when its size differs or its mtime is at or after the index start minus 2 s, so a same-size edit whose mtime was set back (`cp -p`, `rsync -t`, `tar x`, `touch -d`) was missed: with servers off a deleted Rust declaration was answered `disabled` from heuristic edges instead of `stale_target`, a pre-computed answer stayed `lsp-indexed`, and `explore_file` listed the indexed entities without the stale note, while `status` reported the file changed. The new `EditedFiles::is_target_changed` hashes the target's own file whatever its size and mtime (once per call); `Describer::file_changed`, the own-file rule of `indexed_is_current` and `explore_file`'s stale check use it. Other changed files are still found by the coarse filters.

## [0.7.5] - 2026-11-17

### Fixed
- **A deleted or unreadable target file is `stale_target` with a language server on** ([#17](https://github.com/raultov/knot-lite/issues/17)). When the target's indexed file was deleted, replaced by a FIFO or a non-regular file, or swapped for a symlink escaping the repository, `Describer::plan` with a pool failed positioning the request and returned `no_position` (no re-index note), while servers off reported `stale_target`. Both the request-positioning error arm and the re-anchor positioning error arm now run the changed-file check first, so the target is `stale_target` with the refresh note; a missing declaration name in an unchanged file stays `no_position`.
- **Targets in a changed file over 256 KiB are no longer `stale_target`** ([#16](https://github.com/raultov/knot-lite/issues/16)). `reparse_batch` reported a file over `MAX_REPARSE_BYTES` as a generic re-parse failure, which `locate_fresh` mapped to `stale_target` although the declaration may still be there. The size limit now has its own outcome (`ReparseFailure::TooLarge` / `ReparseError::TooLarge`), handled like the per-call re-parse budget: the target keeps its answer — its usual status (`disabled`, `lsp: null`, …) when no server may answer, `reparse_limit` when it needed re-anchoring (or the pre-computed answer marked `outdated`) — and a note names the file as too large to re-parse. Missing files, parser failures and declarations not found in the changed file stay `stale_target`.

## [0.7.4] - 2026-11-16

### Added
- **Pinned Rust toolchain in `rust-toolchain.toml` and `rust-version` in `Cargo.toml`** ([#12](https://github.com/raultov/knot-lite/issues/12)). `rust-toolchain.toml` pins `channel = "1.100.0"` with `rustfmt`, `clippy` and `rust-analyzer`, matching `.github/workflows/ci.yml` (`RUST_TOOLCHAIN`, checked by `make check`), so local builds and CI run the same compiler and lints; `Cargo.toml` declares `rust-version = "1.98"`.

### Changed
- **Upgrade `knot` to 1.16.2** ([#15](https://github.com/raultov/knot-lite/issues/15)). `KNOT_VERSION` and the E2E manifest checks follow. Build-only upstream release; indexes built with 1.16.1 still load. Not re-measured.

### Fixed
- **Callers of a deleted declaration in a language without a server are `stale_target`** ([#8](https://github.com/raultov/knot-lite/issues/8)). `Describer::plan` returned `lsp: null` for languages with no language server (such as Python) before checking whether the target's file changed, and `EditedFiles` only walks LSP-language files. Non-LSP target files are now checked against `file_sha16` and re-parsed when changed, so a deleted declaration is marked `stale_target` with the refresh note; a changed file that keeps the declaration still reports `lsp: null`.
- **Deleted declarations with a pre-computed answer are `stale_target`, not `lsp-indexed` (`outdated`)** ([#9](https://github.com/raultov/knot-lite/issues/9)). When a target had a pre-computed language-server answer and its declaration was deleted from its file, `Describer::plan` fell back to the outdated pre-computed answer both with servers off and after a stale re-anchor result. The changed-file re-parse check now runs before falling back to `lsp-indexed` (`outdated`), and a `stale_target` re-anchor result is preserved.
- **`explore_file` and re-parse refuse symlinks that swap an indexed file for one outside the repository** ([#10](https://github.com/raultov/knot-lite/issues/10)). Replacing an indexed file with a symlink pointing outside the repository root caused `explore_indexed` and `reparse_batch` to read and outline the external file. `reparse_batch`, `SourceFiles`, `EditedFiles`, `ExploreLive::outline`, and `open_target` now check `CanonicalRoot::contains`, so an indexed file replaced by an escaping symlink is treated as stale and described from the index without reading or outlining the external target.
- **`explore_file` no longer outlines files that the index skipped as too large or binary** ([#11](https://github.com/raultov/knot-lite/issues/11)). `on_disk_candidate` now applies `scan::exclude_reason` (`MAX_FILE_BYTES` = 500 KiB, binary sniff, lock-file names) in addition to the gitignore walk, returning `not_found` without asking a language server.
- **`find_callers` and `explore_file` no longer hang when an indexed file is replaced by a FIFO** ([#13](https://github.com/raultov/knot-lite/issues/13)). `EditedFiles::compute_changed`, `file_sha16`, `reparse_batch`, `SourceFiles`, `ExploreLive::outline`, `open_target`, `LspSession::did_open` and `sync_before` now require `is_file()` before opening a path on disk, treating a FIFO or other non-regular file in place of an indexed file as deleted/changed instead of blocking on `open()`.
- **README capability table states the 500 KiB index limit** ([#14](https://github.com/raultov/knot-lite/issues/14)). Tier 0 (`Excluded`) now documents `> 500 KiB` (`MAX_FILE_BYTES`), distinguishing it from the 256 KiB tier-1 and query-time re-parse limits (`MAX_TIER1_BYTES`, `MAX_REPARSE_BYTES`).

## [0.7.3] - 2026-11-15

### Fixed
- **Targets past the re-parse limit are no longer `stale_target`** ([#6](https://github.com/raultov/knot-lite/issues/6)). With more than 32 changed target files in one `find_callers` call, the files past the per-call re-parse limit were reported as deleted declarations. Such a target now keeps its answer: its usual status (`disabled`, …) when no server may answer, `reparse_limit` when it needed re-anchoring (or the pre-computed answer marked `outdated`), and a note counts these targets. The note about changed caller files attributed through the last index loses its `language-server query reparse_limit:` prefix.
- **`explore_file` no longer outlines gitignored files** ([#7](https://github.com/raultov/knot-lite/issues/7)). An unindexed path is only outlined by the language server when the index selection could reach it (git ignore rules, excluded directories); otherwise it is `not_found`, as with servers off.

## [0.7.2] - 2026-11-15

### Fixed
- **Callers of a deleted declaration are `stale_target` with LSP off too** ([#1](https://github.com/raultov/knot-lite/issues/1)). A target no language server may answer (live LSP disabled, unsupported kind, no column) whose declaration was deleted from a changed file was answered from the stale index as `disabled`. Its file is now re-parsed and the target is marked `stale_target`, with the "changed since the last index" note, as with a server on.
- **`explore_file` no longer outlines files outside the repository through a symlink** ([#2](https://github.com/raultov/knot-lite/issues/2)). An unindexed path whose resolved target is outside the root is `not_found`, and no language server is asked.
- **A live Rust query no longer writes `Cargo.lock` into the repository** ([#3](https://github.com/raultov/knot-lite/issues/3)). rust-analyzer's `cargo metadata` / `cargo check` resolve through `<scratch>/cargo-lock/Cargo.lock` (`CARGO_RESOLVER_LOCKFILE_PATH`, a copy of the repository's lockfile refreshed at every start). Needs cargo 1.97 or newer.
- **`index` skips file symlinks that resolve outside the repository** ([#4](https://github.com/raultov/knot-lite/issues/4)), like escaping directory symlinks already were: their content no longer reaches `.knot-lite/`.
- **`knot-lite-mcp` answers a non-JSON line with the JSON-RPC parse error** ([#5](https://github.com/raultov/knot-lite/issues/5)) (`-32700`, `id: null`) and keeps serving, instead of dropping it silently. knot-lite now ships its own stdio transport (`mcp_transport`); JSON that is not a client message is logged to stderr and ignored.

## [0.7.1] - 2026-11-14

### Changed
- **Upgrade `knot` to 1.16.1.** `KNOT_VERSION` follows. knot 1.16.1 only changes knot's own MCP tools (`explore_file` no longer returns a partial `structuredContent`, [knot-server#12](https://github.com/raultov/knot-server/issues/12), and the tool descriptions list every indexed language family); knot-lite serves its own tools, so indexing and query results are unchanged. Indexes built with 1.16.0 still load. Not re-measured.

## [0.7.0] - 2026-11-14

### Added
- **`ort-load-dynamic` cargo feature** ([knot#54](https://github.com/raultov/knot/issues/54)). It forwards knot's feature of the same name: knot-lite loads Microsoft's official ONNX Runtime shared library (1.27 or newer) from `ORT_DYLIB_PATH` instead of linking the prebuilt static one, which needs a CPU with AVX and glibc 2.38. A missing or too old library stops `index` and searches with an error naming `ORT_DYLIB_PATH`. See README "CPUs without AVX, glibc older than 2.38".

### Changed
- **Upgrade `knot` to 1.16.0** (from 1.11.3). `KNOT_VERSION` follows. knot's resolver now also emits `ROUTES_TO` edges (HTTP endpoint → handler, Java/Kotlin), so `ROUTES_TO` joins the edge labels, last in their order. Indexes built with 1.11.3 still load; `status` reports the knot version they were built with. Not re-measured: the 0.6.1 figures were taken with knot 1.11.3.
- **`make build` / `make install` use default features.** `--all-features` would now turn on `ort-load-dynamic`, and the binaries would then need the shared library at run time.

## [0.6.1] - 2026-10-19

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
