# Changelog

All notable changes to `knot-lite` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
