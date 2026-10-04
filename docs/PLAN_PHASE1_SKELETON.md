# PLAN — knot-lite Phase 1: skeleton, quality gates and the `status` vertical slice

> **Audience:** autonomous implementation agents (Build agents) executing without access to the
> conversation that produced this plan. Everything needed is in this file.
> **Status:** ready for execution. **Plan revision:** 0. **Date:** 2026-10-04.
> **Owner (human):** Raul Tovar. **Repository:** `/home/raul/workspace/rust/knot-lite`
> (renamed by the human from `knot-rag` before execution — see §0).
> **Inputs:** `docs/PLAN_PHASE0_SPIKES_0.md` (esp. Appendix F) and
> `docs/spikes/phase0-results.md` (esp. §1, §14, §16).

---

## Table of contents

0. [Preconditions and the rename](#0-preconditions-and-the-rename)
1. [Goal and non-goals](#1-goal-and-non-goals)
2. [Phase 0 evidence this phase relies on](#2-phase-0-evidence-this-phase-relies-on)
3. [Decisions (do not re-litigate)](#3-decisions-do-not-re-litigate)
4. [Verified facts (APIs, environment)](#4-verified-facts-apis-environment)
5. [Ground rules for executing agents](#5-ground-rules-for-executing-agents)
6. [Target layout](#6-target-layout)
7. [Task graph, split, effort](#7-task-graph-split-effort)
8. [Tasks P1.0 – P1.15](#8-tasks)
9. [Output contracts (status JSON, messages, exit codes)](#9-output-contracts)
10. [Risks and pitfalls](#10-risks-and-pitfalls)
11. [Definition of done](#11-definition-of-done)
12. [Hand-off to Phase 2](#12-hand-off-to-phase-2)
- [Appendix A — MCP tool schemas](#appendix-a--mcp-tool-schemas)
- [Appendix B — Language label table](#appendix-b--language-label-table)
- [Appendix C — MCP stdio wire examples (2026-07-28)](#appendix-c--mcp-stdio-wire-examples-2026-07-28)
- [Appendix D — Execution notes](#appendix-d--execution-notes)

---

## 0. Preconditions and the rename

1. **Human action (before any agent starts):**
   `mv ~/workspace/rust/knot-rag ~/workspace/rust/knot-lite`. Verified safe: no spike file
   contains the absolute repository path; the spike's path dependency `../../../../knot` is
   relative. Expected side effect: the spike's `target/` (7.8 GiB) rebuilds once (cargo
   fingerprints embed absolute paths). `.idea/` and `.knot/` may need re-opening in the IDE.
2. **Agent check (stop and report if any fails):**
   - `pwd` → `/home/raul/workspace/rust/knot-lite`
   - `ls ../knot/Cargo.toml` exists (path dependency target, knot 1.11.x)
   - `git log --oneline | wc -l` → `0` (no commits yet) **or** P1.0 already done
   - `rustc --version` → 1.98.x; `cargo dupes --version` → 0.2.1
   - `ls docs/PLAN_PHASE0_SPIKES_0.md docs/spikes/phase0-results.md` both exist
3. **Naming.** From this phase on the product is **knot-lite**. Phase 0 artefacts
   (`docs/PLAN_PHASE0_SPIKES_0.md`, `docs/spikes/phase0-results.md`, the spike crate
   `knot-rag-spike-phase0`, JSON schemas `knot-rag-spike/*`, `/tmp/knot-rag-spike`) are a
   historical record and are **not** renamed. Phase 1 adds one note to the results report (P1.13).

## 1. Goal and non-goals

### 1.1 Goal

Create the knot-lite root crate that mirrors knot's architecture (logic in `cli_tools/run_*`, thin
`mcp_tools/*`, `mcp_handler` with transport-independent `tools()` + `dispatch()`, minimal
binaries). The crate is green under knot's quality gates from its first line and ships one real
end-to-end feature that does not depend on any Phase 2 decision:

- **`status`** — per-repository capability matrix (tier 3/2/1/0 per language, LSP binary
  detection with install hints, index state), exposed identically by the CLI (`knot-lite status`)
  and MCP (`status` tool).
- The four query tools (`search_hybrid_context`, `find_callers`, `explore_file`, `list_files`)
  are **registered with knot-identical input schemas**, validate their arguments and answer with
  an honest, typed "index not available" diagnostic.

### 1.2 Non-goals (Phase 2+)

No `index` command, no embeddings, no shard/edge writing, no `.knot-lite/` creation, no LSP
process spawning or readiness, no graph or vector queries, no HTTP transport, no E2E suite
(`tests/run_all_e2e.sh` is Phase 7), no publishing, no push.

## 2. Phase 0 evidence this phase relies on

| Fact (from `phase0-results.md`) | Consequence for Phase 1 |
|---|---|
| LSP is the default graph for rust/ts/java (G1–G4 pass; recall 100/97/95 %) | tier-3 candidates = exactly these three languages (rust-analyzer, typescript-language-server, jdtls) |
| Degradation path (G16): missing binary → human diagnostic with install hint | `status` reports `Missing{searched_dirs, hint}`, porting the spike's lookup logic (`docs/spikes/phase0/src/lsp/servers.rs`: `which_in`, `path_dir_count`, `install_hint`) |
| Index layout `<repo>/.knot-rag/` (Appendix F.3) → now `.knot-lite/`; `manifest.toml` | Phase 1 only **reads** `.knot-lite/manifest.toml` to report the index state |
| Encoding `i8pv_b64`, commit default `gitignore` (§20.2) | Not used in Phase 1 (Phase 2 writes the manifest) |
| Tier model 3/2/1/0 (Phase 0 §2) | Implemented in `capability/tier.rs` with the Phase 1 caveat "tier 3 = server binary found, not started" |

## 3. Decisions (do not re-litigate)

Phase 0 decisions D1–D11 still apply (`PLAN_PHASE0_SPIKES_0.md` §3). New decisions:

| # | Decision |
|---|---|
| P1-D1 | **Name `knot-lite`.** Package `knot-lite`, lib `knot_lite`, binaries `knot-lite` (CLI) and `knot-lite-mcp` (stdio MCP server). Index dir `.knot-lite/`. Env prefix `KNOT_LITE_`. Config file `~/.config/knot-lite/.env`. MCP `serverInfo.name = "knot-lite"`. |
| P1-D2 | **Scope:** skeleton + `status` vertical slice (§1.1). |
| P1-D3 | **Tool surface = knot's names with knot's exact input fields and constraints** for `search_hybrid_context`, `find_callers`, `explore_file`, `list_files`, plus the new `status`. No `list_repositories`, no `list_repo_dependencies`. CLI subcommands mirror knot: `search`, `callers`, `explore`, `files`, plus `status`. |
| P1-D4 | **Git:** root `.gitignore`; local commit of the Phase 0 baseline on `master`; all Phase 1 work on branch `phase-1`, one commit per task (`phase1(P1.x): <summary>`). **Never push, never tag, never merge into `master`.** |
| P1-D5 | **`dupes.toml`:** add `"docs/spikes/"` to `exclude`. No threshold changes. Reason: cargo-dupes 0.2.1 walks the whole tree (`dupes-core-0.2.1/src/scanner.rs`: `WalkDir`, skips only `target` and hidden dirs), so the root gate would scan the spike and flag spike↔src duplicates when Phase 2 ports spike code. The spike stays checkable with `make check CRATE_DIR=docs/spikes/phase0`. |
| P1-D6 | **MCP SDK:** `rust-mcp-sdk = { version = "2.0", default-features = false, features = ["server", "macros", "stdio"] }`. Stdio only. No axum/actix/auth/sse/streamable-http. |
| P1-D7 | **One server = one repository.** No `roots` in MCP 2026-07-28. Repo path resolution: `--repo-path` → `KNOT_LITE_REPO_PATH` → `~/.config/knot-lite/.env` → current working directory. Repo name: `--repo-name` → `KNOT_LITE_REPO_NAME` → `.env` → last component of the canonical path. |
| P1-D8 | **knot dependency:** `knot = { path = "../knot" }` with **default features** (knot's `src/` has no `#[cfg(feature)]` gates, so it does not build without `indexer`). |
| P1-D9 | `publish = false` with a comment: the path dependency blocks publishing until the knot-parser split (Phase 0 Appendix F.7) is agreed. |
| P1-D10 | **No process-env mutation.** `.env` is read with `dotenvy::from_path_iter` into a map and merged by a pure resolver; `std::env::set_var` (unsafe in edition 2024) is never called. |
| P1-D11 | **Phase 1 never writes into the analysed repository.** `status` is read-only (enforced by tests). |
| P1-D12 | **Query tools fail as tool errors** (`CallToolError` → `isError: true`), never as protocol errors, so the LLM can read the diagnostic and self-correct (2026-07-28 spec guidance on `isError`). |
| P1-D13 | **One stdio integration test** (`tests/mcp_stdio.rs`) is the only test outside `src/`; the transport cannot be covered by lib tests. It is part of the gate: `make check` runs every `cargo test` target (P1-D16). |
| P1-D14 | **Version `0.1.0`.** Edition 2024. MIT. Authors as knot. |
| P1-D15 | **Root `AGENTS.md` is created** (P1.13): Phases 2–7 are executed by agents that need the conventions without reading every plan. *(Confirmed by the human, 2026-10-04.)* |
| P1-D16 | **The Makefile gate covers everything `cargo test` covers.** `make test` = `cargo test --all-features` (library unit tests, unit tests in binaries, integration tests under `tests/`, doctests); `make test-lib` is the fast library-only loop and is not a gate. Already applied to the root `Makefile` on 2026-10-04 and verified green on the spike crate (89 lib tests + 3 binaries + doctests). Nothing may pass `make check` that `cargo test` / the validator would fail. *(Decided by the human, 2026-10-04.)* |

## 4. Verified facts (APIs, environment)

Verified on 2026-10-04. If any is no longer true, **stop and report**.

| Fact | Value / evidence |
|---|---|
| Root repo state | No commits on `master`. Untracked: `.idea/`, `.knot/` (417 MiB fastembed cache), `Makefile`, `clippy.toml`, `dupes.toml`, `docs/`. **No root `.gitignore`** (Phase 0 T0.1 claimed it; it does not exist — log as deviation). |
| `docs/` sizes | `docs/spikes/phase0/results` 31 MiB (two Java S1 JSONs > 5 MiB), `groundtruth` 5.4 MiB, spike `target/` 7.8 GiB (ignored). |
| Root `Makefile` | `CRATE_DIR ?= $(if $(wildcard Cargo.toml),.,docs/spikes/phase0)` → auto-switches to `.` once `Cargo.toml` exists. `BINS := knot-rag knot-rag-mcp` (to rename). `check: fmt-check clippy test dupes`. `test` = `cargo test --all-features` (all targets `cargo test` runs; P1-D16); `test-lib` = `cargo test --lib --all-features` (not a gate). `e2e` skips when `tests/run_all_e2e.sh` is absent. |
| Root `clippy.toml` | `too-many-arguments-threshold = 5`, `type-complexity-threshold = 200` (later target 150), `excessive-nesting-threshold = 6` (later target 5), `too-many-lines-threshold = 100`, `cognitive-complexity-threshold = 20`. Inherited by every crate below through clippy's parent lookup. |
| Root `dupes.toml` | `min_lines = 10`, `min_nodes = 10`, `similarity_threshold = 0.8`, `exclude_tests = true`, `exclude = ["tests/", "benches/", "tests.rs"]`, all four `max_*` = 0. Read **only** from the analysed path. |
| knot lint tables | `[lints.rust] unsafe_code = "deny"`; `[lints.clippy] too_many_lines`, `cognitive_complexity`, `allow_attributes`, `allow_attributes_without_reason` = `"warn"`, with comments. Copy byte-for-byte from `../knot/Cargo.toml`. |
| knot public APIs used | `knot::pipeline::input::discover_files(repo_path: &str, include_config_files: bool) -> anyhow::Result<Vec<PathBuf>>` (respects `.gitignore`/global/exclude, `hidden(false)`, skips files > 500 KiB and lock files, sorted, absolute when given an absolute root). `knot::pipeline::input::SUPPORTED_EXTENSIONS: &[&str]` (pub). `knot::pipeline::files::{is_supported_file, to_repo_relative}`. |
| knot private constants to mirror | `MAX_FILE_SIZE = 500 * 1024`; `EXCLUDED_NAMES = [package-lock.json, yarn.lock, pnpm-lock.yaml, Cargo.lock, composer.lock, Gemfile.lock, poetry.lock, Pipfile.lock]`; `BUILD_SYSTEM_NAMES = [Jenkinsfile, pom.xml, Cargo.toml, package.json, tsconfig.json, Directory.Packages.props]` (`src/pipeline/input.rs`). |
| knot conventions to mirror | `OutputFormat { Table (default), Json, Markdown }` (clap `ValueEnum`). Tables use `comfy-table` with `ContentArrangement::DynamicFullWidth` and coloured headers (`src/cli_tools/repos.rs`). CLI logging defaults to `error`, MCP to `info`, both on **stderr**, overridable by `RUST_LOG`. `.env` path resolution: `KNOT_CONFIG_DIR` → `$HOME/.config/knot/.env` → `$USERPROFILE/.config/knot/.env`, never the cwd. |
| rust-mcp-sdk 2.0.0 (27 Aug 2026) | Implements MCP **2026-07-28** (stateless). Features: `server`, `client`, `macros`, `stdio`, `sse`, `streamable-http`, `auth` (all default). `ServerHandler` methods take `context: &RequestContext` before `runtime: Arc<dyn McpServer>`. `handle_call_tool_request(...) -> Result<ServerResult, CallToolError>`; `handle_list_tools_request(...) -> Result<ListToolsResult, RpcError>`. Server identity is `ServerDetails { server_info: Implementation, capabilities, instructions, meta }` (no `protocol_version`). `McpServerOptions { server_details, transport, handler, message_observer }`. `server_runtime::create_server(..)`, `StdioTransport::new(TransportOptions::default())`, `.to_mcp_server_handler()`. |
| rust-mcp-schema 2.0.0 | `CallToolResult { content, is_error, meta, result_type, structured_content }`; ctors `CallToolResult::text_content(Vec<TextContent>)`, `with_structured_content(Map)`, `with_error(CallToolError)`; `From<CallToolError> for CallToolResult` (`is_error = true`); `From<CallToolResult> for ServerResult`. `ListToolsResult { tools, cache_scope, result_type: "complete", ttl_ms, meta, next_cursor }`. `CallToolRequestParams { name, arguments, input_responses, request_state, meta: RequestMetaObject }` (`meta` required). `RequestMetaObject` wire keys `io.modelcontextprotocol/protocolVersion` and `io.modelcontextprotocol/clientCapabilities` (both required); `RequestMetaObject::new(version, ClientCapabilities)`. Server discovery method: `server/discover`. |
| rust-mcp-macros 2.0.0 | `#[mcp_tool(name, title, description, read_only_hint, destructive_hint, idempotent_hint, open_world_hint, meta, icons)]` + `#[derive(JsonSchema)]` generate `Tool::tool()`, `tool_name()`, `request_params()`. The `execution` attribute was removed in 2.0. Field attributes `#[json_schema(description, min_length, max_length, minimum, maximum, default)]` are used by knot on 1.x — **verify in P1.3**. |
| Dependency graph | knot depends on `rust-mcp-sdk 1.1`, knot-lite on `2.0` → both majors coexist (legal, longer build). The `#[mcp_tool]` expansion refers to `rust_mcp_sdk`, which in knot-lite resolves to the direct 2.0 dependency. |
| Pre-existing change in knot | `../knot` has ` M Dockerfile` (the human's edit). Phase 1 never writes into `../knot`. |

## 5. Ground rules for executing agents

1. **Write only inside** `/home/raul/workspace/rust/knot-lite/` (plus `/tmp/knot-lite-*` scratch
   dirs). Never write into `../knot`, `~/.knot/repos/*` or any other repository. Read-only runs of
   `knot-lite status` on `~/.knot/repos/*` are allowed (P1.15).
2. Rust rules: edition 2024; **no `unsafe`**; **no `#[allow]`**; `#[expect(..., reason = "...")]`
   only as a last resort and reported to the human; fix clippy/dupes findings by refactoring.
   Never loosen `clippy.toml`/`dupes.toml`; never add `.dupes-ignore.toml` entries except the two
   legitimate categories of knot's `AGENTS.md`, each with a `reason`.
3. All code, comments and docs in **English**.
4. Cross-platform: paths via `std::path`; no shelling out from Rust (exception: the P1.6 test
   fixture runs `git init` inside its own tempdir); Windows executable lookup via `PATHEXT` (P1.6).
5. **Unit tests inside library modules** (`#[cfg(test)] mod tests`). Binaries stay thin (their
   logic lives in the library, where it is unit-tested). The only test outside `src/` is
   `tests/mcp_stdio.rs` (P1-D13). `make check` runs all of them (P1-D16).
6. After every task: `make check` from the repository root must be green before committing. At
   the end (P1.15) also the `validator` subagent.
7. Use the knot MCP tools (`search_hybrid_context`, `find_callers`, `explore_file`) when reading
   `../knot` code.
8. If this plan is wrong about an API or a path, fix it conservatively and record the deviation
   in Appendix D.
9. Never commit `target/`, `.knot/`, `.idea/`, `docs/spikes/phase0/results/raw/`. Stage files
   explicitly (no `git add -A`).

## 6. Target layout

```
knot-lite/
├── .gitignore                       # P1.0
├── Cargo.toml  Cargo.lock           # P1.2 (lockfile committed: binaries)
├── Makefile  clippy.toml  dupes.toml  # P1.1 (headers/BINS/exclude), P1.14 (curves)
├── README.md                        # P1.13
├── AGENTS.md                        # P1.13 (P1-D15)
├── docs/
│   ├── PLAN_PHASE0_SPIKES_0.md      # historical
│   ├── PLAN_PHASE1_SKELETON.md      # this file
│   └── spikes/…                     # historical (excluded from the root dupes gate)
├── tests/
│   └── mcp_stdio.rs                 # P1.12 (only test outside src/)
└── src/
    ├── lib.rs                       # module list + crate docs
    ├── config.rs                    # P1.4  Config, OutputFormat, CommonArgs, resolver
    ├── error.rs                     # P1.7  QueryUnavailable (typed diagnostics)
    ├── utils.rs                     # P1.4  logging (stderr)
    ├── models/
    │   ├── mod.rs
    │   ├── cli_args.rs              # P1.8  Cli, Commands
    │   ├── params.rs                # P1.8  SearchParams, CallersParams, ExploreParams, FilesParams
    │   └── repo_scope.rs            # P1.9  single-repository scope check
    ├── index/
    │   ├── mod.rs
    │   ├── layout.rs                # P1.5  INDEX_DIR, MANIFEST_FILE, IndexLayout
    │   └── manifest.rs              # P1.5  ManifestHeader, IndexState, read_index_state
    ├── capability/
    │   ├── mod.rs
    │   ├── languages.rs             # P1.6  label_for(path), LspLang, Appendix B
    │   ├── which.rs                 # P1.6  PATH/PATHEXT lookup (ported from the spike)
    │   ├── lsp_registry.rs          # P1.6  server specs, overrides, install hints
    │   ├── scan.rs                  # P1.6  file classification
    │   └── tier.rs                  # P1.6  TierRow, StatusReport assembly
    ├── cli_tools/
    │   ├── mod.rs                   # P1.11 run_cli, exit_code_for
    │   ├── status.rs                # P1.7  run_status, format_status_output
    │   └── query.rs                 # P1.7  run_search_hybrid_context / run_find_callers / run_explore_file / run_list_files
    ├── mcp_tools/
    │   ├── mod.rs                   # P1.9  parse_args, query_tool
    │   ├── search_hybrid_context.rs
    │   ├── find_callers.rs
    │   ├── explore_file.rs
    │   ├── list_files.rs
    │   └── status.rs
    ├── mcp_handler.rs               # P1.10 KnotLiteMcpHandler, tools(), dispatch(), build_server_details()
    └── bin/
        ├── knot-lite.rs             # P1.11
        └── knot-lite-mcp.rs         # P1.11
```

## 7. Task graph, split, effort

```
P1.0 git baseline ─► P1.1 gate files ─► P1.2 Cargo.toml + lib.rs ─► P1.3 SDK probe ──────────────────────────────┐
                                              │                                                                  │
                                              └─► P1.4 config+utils ─► P1.5 index ─► P1.6 capability ─► P1.7 cli_tools ─► P1.8 cli_args
                                                                                                                 │
                                                                     P1.9 mcp_tools (needs P1.3 + P1.7 + P1.8) ◄─┘
                                                                            │
                          P1.10 handler ─► P1.11 bins ─► P1.12 stdio test ─► P1.13 docs ─► P1.14 clippy curves ─► P1.15 gate + hand-off
```

| Task | Effort | Agent (if split) |
|---|---|---|
| P1.0 git baseline | 0.3 h | A |
| P1.1 gate files | 0.5 h | A |
| P1.2 Cargo.toml + lib | 0.5 h (+ first knot build: minutes, ort download) | A |
| P1.3 SDK probe | 1 h | B |
| P1.4 config + utils | 2 h | A |
| P1.5 index layout/manifest | 1 h | A |
| P1.6 capability | 3.5 h | A |
| P1.7 cli_tools | 2 h | A |
| P1.8 cli_args + params | 1 h | A |
| P1.9 mcp_tools | 2.5 h | B |
| P1.10 handler | 1.5 h | B |
| P1.11 bins | 1 h | B |
| P1.12 stdio integration test | 1.5 h | B |
| P1.13 README / AGENTS / report note | 1 h | A |
| P1.14 clippy curves | 1 h | A |
| P1.15 gate + smoke + hand-off | 1 h | A |
| **Total** | **≈ 22 h** | |

One agent is sufficient. If split, agent **A owns `Cargo.toml`**; B requests dependency changes
through A. B starts P1.3 right after P1.2.

---

## 8. Tasks

### P1.0 — Git baseline

**Steps**
1. Create `.gitignore` at the repository root:
   ```gitignore
   /target/
   **/target/
   docs/spikes/phase0/results/raw/
   .knot/
   .idea/
   ```
2. Stage explicitly: `git add .gitignore Makefile clippy.toml dupes.toml docs/`.
3. `git status --short` must not list anything under `target/`, `.knot/`, `.idea/`, `results/raw/`.
4. Commit on `master`: `Phase 0: feasibility spikes (S1 LSP graph, S2 file-based vectors) and results`.
5. `git switch -c phase-1`.
6. Record the missing-`.gitignore` deviation in Appendix D.

**Acceptance:** `git log --oneline` shows one commit on `master`; current branch `phase-1`;
`git status` clean.

### P1.1 — Adapt the root gate files

**Steps**
1. `Makefile`
   - The comment above `CRATE_DIR`: state that the root crate now exists and that the spike is
     checked with `make check CRATE_DIR=docs/spikes/phase0`.
   - `BINS := knot-lite knot-lite-mcp`.
   - The `install` error text says "knot-lite binaries"; the `knot-rag binaries` comment above
     `BINS` says `knot-lite`.
   - **Keep** the `test` / `test-lib` targets as they are (already adapted before Phase 1, P1-D16).
   - No other change.
2. `clippy.toml`: header `Clippy thresholds for knot-rag.` → `knot-lite`; replace every other
   `knot-rag` mention with `knot-lite`. **No value changes** (P1.14 handles curves).
3. `dupes.toml`
   - Header → `knot-lite`.
   - `exclude = ["tests/", "benches/", "tests.rs", "docs/spikes/"]`, with this comment above it:
     ```toml
     # docs/spikes/ holds throw-away spike crates with their own [workspace]. cargo-dupes
     # walks the whole tree from --path (it only skips `target` and hidden directories), so
     # without this exclusion the root gate would analyse the spikes and flag spike <-> src
     # duplicates whenever spike code is ported. Spikes are gated separately with
     # `make check CRATE_DIR=docs/spikes/phase0` (they carry a dupes.toml symlink).
     # NOTE: plain substring match on the path; forward slashes (gate runs on Linux/macOS).
     ```
   - The existing comment about `docs/spikes/phase0/dupes.toml -> ../../../dupes.toml` stays; add
     "(the spike's own gate)".

**Acceptance** (run after P1.2 creates `Cargo.toml`; record outputs in Appendix D)
- `cargo dupes --path . report` lists **no** path containing `docs/spikes/`.
- `make check CRATE_DIR=docs/spikes/phase0` is still green (the first run rebuilds after the rename).
- `grep -rn "knot-rag" Makefile clippy.toml dupes.toml` → no matches.

### P1.2 — Root `Cargo.toml` and `src/lib.rs`

**`Cargo.toml`** (exact content apart from the lint block, copied byte-for-byte from `../knot/Cargo.toml`):
```toml
[package]
name = "knot-lite"
version = "0.1.0"
edition = "2024"
description = "Database-free code intelligence for one repository: file-based vectors + language-server call graph (built on knot's parsers)"
authors = ["Raul Tovar <raultov@protonmail.com>"]
license = "MIT"
# The path dependency on knot blocks publishing until the knot-parser split
# (PLAN_PHASE0_SPIKES_0.md Appendix F.7) is agreed with the human.
publish = false

# <<< [lints.rust] and [lints.clippy] copied VERBATIM from ../knot/Cargo.toml (D11) >>>

[[bin]]
name = "knot-lite"
path = "src/bin/knot-lite.rs"

[[bin]]
name = "knot-lite-mcp"
path = "src/bin/knot-lite-mcp.rs"

[dependencies]
# Parsers, file discovery, entity model (D3). Default features are required:
# knot's src/ does not cfg-gate fastembed/ort behind its `indexer` feature.
knot = { path = "../knot" }

# --- MCP server (stateless MCP 2026-07-28, stdio only) ---
rust-mcp-sdk = { version = "2.0", default-features = false, features = ["server", "macros", "stdio"] }
async-trait = "0.1"
tokio = { version = "1", features = ["full"] }

# --- Configuration & CLI ---
clap = { version = "4", features = ["derive", "env"] }
dotenvy = "0.15"

# --- Filesystem traversal (same major as knot) ---
ignore = "0.4"

# --- Serialization ---
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "1"

# --- Output formatting (same version as knot) ---
comfy-table = "7.1"

# --- Errors & logging ---
anyhow = "1"
thiserror = "2"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }

[dev-dependencies]
tempfile = "3"

[profile.release]
lto = true
codegen-units = 1
opt-level = 3
```
**Byte-identity check:** extract the block from `[lints.rust]` to
`allow_attributes_without_reason = "warn"` from both files with `awk` and `diff` them; the diff
must be empty. Record the command in Appendix D.

**`src/lib.rs`**
```rust
//! knot-lite — database-free code intelligence for one repository.
//!
//! Mirrors knot's architecture: business logic in `cli_tools::run_*`, thin MCP
//! wrappers in `mcp_tools`, a transport-independent `mcp_handler`
//! (`tools()` + `dispatch()`), and minimal binaries.

pub mod capability;
pub mod cli_tools;
pub mod config;
pub mod error;
pub mod index;
pub mod mcp_handler;
pub mod mcp_tools;
pub mod models;
pub mod utils;
```
Create every module file of §6 with a module doc comment (`//! …`) and no items yet, plus
minimal `fn main() {}` binaries, so the crate builds.

**Acceptance:** `cargo build` succeeds (the first build compiles knot incl. fastembed/ort/qdrant/
neo4rs — minutes); `make check` runs with `CRATE_DIR=.` (no argument) and is green. Commit.

### P1.3 — rust-mcp-sdk 2.0 API probe (before any MCP code)

**Goal:** turn every API assumption of §4 into a verified fact.

**Steps**
1. Locate sources: `ls -d ~/.cargo/registry/src/*/rust-mcp-{sdk,schema,macros,transport}-2.*`.
2. Read and confirm (record exact signatures and paths in Appendix D):
   - `rust_mcp_sdk::mcp_server::{ServerHandler, server_runtime, McpServerOptions}`; the field list of `McpServerOptions`.
   - Paths of `ServerDetails`, `RequestContext`, `McpServer`, `StdioTransport`, `TransportOptions`, `ToMcpServerHandler`.
   - The exact `handle_list_tools_request` / `handle_call_tool_request` signatures.
   - `ListToolsResult` field names, the type of `cache_scope` and its `Default`.
   - `CallToolError` constructors (`unknown_tool`, `from_message`, any `invalid_arguments`) and its `Display` format; whether `SdkError::rpc_error_message` still exists.
   - `CallToolResult::text_content`, `with_structured_content`; `TextContent::new` arity.
   - `#[mcp_tool]` / `#[derive(JsonSchema)]` / `#[json_schema(...)]` support for `description`, `min_length`, `max_length`, `minimum`, `maximum`, `default` under `default-features = false, features = ["server","macros","stdio"]`; the return type of `tool_name()`.
   - Whether `JsonSchema`-derived tool structs can also derive `serde::Deserialize` (the README example does).
   - Stdio framing: newline-delimited JSON on stdin/stdout; `DEFAULT_MAX_LINE_LENGTH`.
   - What the stdio runtime answers to a request without `_meta`/`protocolVersion` (error code), for the P1.12 negative test.
   - What the runtime answers to `tools/call` of an unknown tool (`isError` result vs JSON-RPC error).
3. Write a throw-away probe under `/tmp/knot-lite-sdk-probe` (own crate, not committed): a
   minimal server with one `#[mcp_tool]` struct using
   `#[json_schema(min_length = 1, max_length = 10)]` and
   `#[json_schema(minimum = 1, maximum = 100, default = 5)]` fields. Print
   `serde_json::to_string_pretty(&Probe::tool())` and check the constraints appear in the schema.
4. If any assumption fails, adapt the P1.9–P1.12 sketches and record the deviation.

**Acceptance:** Appendix D lists the verified signatures; probe output pasted (trimmed).

### P1.4 — `src/config.rs` and `src/utils.rs`

**Types**
```rust
#[derive(Debug, Clone, Copy, clap::ValueEnum, PartialEq, Eq, Default)]
pub enum OutputFormat { #[default] Table, Json, Markdown }

/// Flags shared by both binaries (flattened into their clap structs).
#[derive(Debug, Clone, Default, clap::Args)]
pub struct CommonArgs {
    /// Repository root. Defaults to the current working directory.
    #[arg(long, global = true, env = "KNOT_LITE_REPO_PATH")]
    pub repo_path: Option<String>,
    /// Logical repository name. Defaults to the last component of the repository path.
    #[arg(long, global = true, env = "KNOT_LITE_REPO_NAME")]
    pub repo_name: Option<String>,
}

/// Language-server overrides: program name or path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LspOverrides { pub rust: Option<String>, pub ts: Option<String>, pub java: Option<String> }

#[derive(Debug, Clone)]
pub struct Config {
    pub repo_path: std::path::PathBuf,   // canonical, absolute
    pub repo_name: String,
    pub lsp_overrides: LspOverrides,
    pub dry_run: bool,                   // MCP only
}
```

**Resolution (pure, injectable, fully unit-tested)**
```rust
/// `env` = process-environment lookup; `dotenv` = key/values read from the
/// knot-lite .env file. Precedence: CLI (already in `args`; clap merged the
/// process env) > process env > .env > default.
pub fn resolve(
    args: &CommonArgs,
    dry_run: bool,
    env: &dyn Fn(&str) -> Option<String>,
    dotenv: &std::collections::BTreeMap<String, String>,
    cwd: &std::path::Path,
) -> anyhow::Result<Config>
```
- `lookup(key) = env(key).or_else(|| dotenv.get(key).cloned())`. It is used for the keys clap does
  not see: `KNOT_LITE_LSP_RUST`, `KNOT_LITE_LSP_TS`, `KNOT_LITE_LSP_JAVA`. For `repo_path` and
  `repo_name`, clap already merged CLI and process env into `args`, so fall back to `dotenv`
  only when `args.*` is `None`.
- `repo_path`: the chosen value or `cwd`, then `std::fs::canonicalize`. **Error** if it does not
  exist or is not a directory: `repository path '{p}' does not exist or is not a directory`.
  When `dry_run == true`, skip the existence check and keep the non-canonical path.
- `repo_name`: chosen value, else `canonical.file_name()`, else `"unnamed-repo"` (knot's fallback).
- `env_path_from(get) -> Option<PathBuf>`: `KNOT_LITE_CONFIG_DIR/.env` →
  `HOME/.config/knot-lite/.env` → `USERPROFILE/.config/knot-lite/.env`. **Never the cwd.**
- `read_dotenv(path) -> BTreeMap<String,String>` via `dotenvy::from_path_iter`. `NotFound` →
  empty map plus `debug!`. Any other error → empty map plus `warn!` (knot's behaviour).
- Convenience loaders `Config::load_cli(args: &CommonArgs)` and
  `Config::load_mcp(args: &CommonArgs, dry_run: bool)` wire the real process env, the real
  `.env` and the real cwd into `resolve`.

**`src/utils.rs`**
- `init_logging(default_level: &str) -> anyhow::Result<()>`:
  `tracing_subscriber::fmt().with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level))).with_writer(std::io::stderr).with_ansi(false).try_init()`,
  mapping the error to anyhow.
- Constants `CLI_LOG_LEVEL = "error"`, `MCP_LOG_LEVEL = "info"`.

**Unit tests (`config.rs`)**
1. `env_path_prefers_config_dir`, `env_path_falls_back_to_home`, `env_path_falls_back_to_userprofile`,
   `env_path_none_when_nothing_set`, `env_path_never_cwd`.
2. `resolve_uses_cwd_when_nothing_given` (tempdir as cwd).
3. `resolve_dotenv_used_when_args_and_env_absent`.
4. `resolve_process_env_beats_dotenv_for_lsp_overrides`.
5. `resolve_args_beat_dotenv_for_repo_path`.
6. `resolve_errors_on_missing_path` (message contains the path); `resolve_dry_run_tolerates_missing_path`.
7. `repo_name_defaults_to_dir_name`; `repo_name_explicit_wins`.
8. `read_dotenv_missing_file_is_empty`; `read_dotenv_parses_pairs` (tempfile with `KNOT_LITE_REPO_NAME=x`).

No test touches the real process environment (closures only) → no global mutex needed.

**Acceptance:** green; `make check` green; commit.

### P1.5 — `src/index/{layout,manifest}.rs`

**`layout.rs`**
```rust
pub const INDEX_DIR: &str = ".knot-lite";
pub const MANIFEST_FILE: &str = "manifest.toml";
/// Manifest schema this knot-lite version understands (written from Phase 2 on).
pub const MANIFEST_SCHEMA_V1: &str = "knot-lite/manifest/v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexLayout { pub root: PathBuf, pub dir: PathBuf, pub manifest: PathBuf }
impl IndexLayout { pub fn for_repo(repo_root: &Path) -> Self { … } }
```
Only these constants. The other Appendix F.3 paths (`shards/`, `edges/`, `cache/`,
`.gitattributes`) are defined by Phase 2 together with their writers.

**`manifest.rs`**
```rust
/// The only fields Phase 1 reads; unknown fields are ignored so Phase 2 can
/// extend the manifest without breaking `status`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct ManifestHeader { pub schema: String, pub knot_lite_version: String }

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum IndexState {
    Absent,
    Present { schema: String, knot_lite_version: String },
    Unsupported { schema: String },
    Invalid { error: String },
}

pub fn read_index_state(layout: &IndexLayout) -> IndexState
```
- Manifest missing (`NotFound`) → `Absent`; any other I/O error → `Invalid{error}`.
- TOML parse error or missing fields → `Invalid{error}` (the toml error message, one line).
- `schema != MANIFEST_SCHEMA_V1` → `Unsupported{schema}`; otherwise `Present{…}`.
- **Never** creates directories or files.

**Unit tests:** `absent_when_no_dir`; `absent_when_dir_without_manifest`;
`present_for_v1_manifest` (including extra unknown tables, e.g. `[embedding] model = "x"`);
`unsupported_for_other_schema`; `invalid_for_garbage_toml`; `invalid_for_missing_schema_field`;
`serializes_with_state_tag` (`{"state":"absent"}`); `read_is_side_effect_free` (tempdir listing
before == after).

**Acceptance:** green; commit.

### P1.6 — `src/capability/*`

#### `languages.rs`
- `pub enum LspLang { Rust, Ts, Java }` with `server_program(self) -> &'static str`
  (`rust-analyzer`, `typescript-language-server`, `jdtls`) and `env_override_key(self)`
  (`KNOT_LITE_LSP_RUST` / `_TS` / `_JAVA`).
- `pub fn label_for(path: &Path) -> &'static str`: filename table first, then the lower-cased
  extension, per **Appendix B**; fallback `"other"`.
- `pub fn lsp_lang_for_label(label: &str) -> Option<LspLang>`: `rust`→Rust;
  `typescript`/`javascript`→Ts; `java`→Java; others `None`.
- **Drift-guard test** `every_knot_supported_extension_has_a_label`: for each `ext` in
  `knot::pipeline::input::SUPPORTED_EXTENSIONS`, `label_for(Path::new(&format!("x.{ext}")))` is
  not `"other"`. When knot adds a language this test fails and forces an update of Appendix B.
- One test per notable row: `.tsx`→typescript, `.mjs`→javascript, `Cargo.toml`→cargo,
  `pom.xml`→maven, `Jenkinsfile`→jenkins, `.H` (upper case)→c_cpp, `Makefile`→other.

#### `which.rs` (port from `docs/spikes/phase0/src/lsp/servers.rs`, then extend)
```rust
/// Resolve `program` against an explicit PATH value. With Windows semantics
/// (`windows = true`) each PATHEXT extension (default ".COM;.EXE;.BAT;.CMD")
/// is also tried when `program` has no extension. Pure: `is_exec` is injected.
pub fn which_in(
    program: &str,
    path_var: Option<&OsStr>,
    pathext: Option<&OsStr>,
    windows: bool,
    is_exec: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf>
pub fn which(program: &str) -> Option<PathBuf>      // real env, cfg!(windows), is_executable
pub fn path_dir_count(path_var: Option<&OsStr>) -> usize
pub fn is_executable(path: &Path) -> bool           // unix: is_file && mode & 0o111 != 0; other: is_file
```
- A value containing a path separator, or an absolute path, is checked directly with `is_exec`
  (no PATH search).
- `#[cfg(unix)] use std::os::unix::fs::PermissionsExt;`; `#[cfg(not(unix))]` → `is_file`.
- **Tests** (synthetic PATH in a tempdir; Windows logic exercised with `windows = true` on Linux):
  `finds_program_in_second_dir`, `none_when_absent`, `none_when_not_executable` (`#[cfg(unix)]`),
  `windows_tries_pathext_in_order` (`tsls.cmd` found for program `tsls`),
  `windows_default_pathext_when_unset`, `program_with_extension_not_extended`,
  `path_value_checked_directly`, `path_dir_count_counts_entries`.

#### `lsp_registry.rs`
```rust
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LspStatus {
    Found { server: String, path: String },
    Override { server: String, path: String },
    OverrideInvalid { server: String, value: String, hint: String },
    Missing { server: String, searched_dirs: usize, hint: String },
}
pub fn detect(lang: LspLang, overrides: &LspOverrides) -> LspStatus
pub fn install_hint(lang: LspLang) -> &'static str
```
- Hints:
  - rust: `` Install it with `rustup component add rust-analyzer`. ``
  - ts: `` Install it with `npm install -g typescript-language-server typescript`. ``
  - java: `Install Eclipse JDT Language Server (jdtls) 1.61+ from https://download.eclipse.org/jdtls/milestones/ and put its bin/ directory on PATH.`
- Precedence is explicit: an override is set → `Override` (valid) or `OverrideInvalid` (no PATH
  fallback, so typos are visible); no override → PATH → `Found` / `Missing`.
- `detect` is a thin wrapper over a pure
  `detect_with(lang, overrides, path_var, pathext, windows, is_exec)`.
- **Tests:** found; missing (`searched_dirs` correct); override valid; override invalid; Windows `.cmd`.

#### `scan.rs`
```rust
pub const MAX_FILE_BYTES: u64 = 500 * 1024;            // mirrors knot input.rs MAX_FILE_SIZE (private)
pub const LOCK_FILE_NAMES: &[&str] = &[ /* knot EXCLUDED_NAMES, same order */ ];
pub const RESERVED_DIRS: &[&str] = &[".git", ".knot", ".knot-lite"];
const SNIFF_BYTES: u64 = 8 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExcludeReason { Binary, TooLarge, LockFile }

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileClass {
    Parsed { label: &'static str },
    FilesOnly { key: String },          // ".sh", ".sql", "Makefile", "Dockerfile", "(no extension)"
    Excluded(ExcludeReason),
}

pub struct ScanResult { pub files: Vec<(String /* repo-relative */, FileClass)> }

pub fn scan_repository(root: &Path) -> anyhow::Result<ScanResult>
```
**Algorithm** (one root string for both walks, so path equality holds):
1. `let root_str = root.to_str().context("repository path is not valid UTF-8")?;`
2. `let parsed: HashSet<PathBuf> = knot::pipeline::input::discover_files(root_str, false)?.into_iter().collect();`
3. Walk with `ignore::WalkBuilder::new(root_str)`:
   - `.hidden(false).git_ignore(true).git_global(true).git_exclude(true)` (identical to knot)
   - `.filter_entry(|e| !(e.depth() > 0 && e.file_type().is_some_and(|t| t.is_dir()) && RESERVED_DIRS.contains(&e.file_name().to_string_lossy().as_ref())))`
4. For each regular file `p`, classify in this order:
   1. `parsed.contains(p)` → `Parsed{label_for(p)}`
   2. file name ∈ `LOCK_FILE_NAMES` → `Excluded(LockFile)`
   3. metadata len > `MAX_FILE_BYTES` → `Excluded(TooLarge)`
   4. the first `SNIFF_BYTES` contain `0u8` → `Excluded(Binary)`. Read through `Read::take`. An I/O
      error counts as binary and is logged at `debug!`.
   5. otherwise → `FilesOnly{key}`, where `key` is `".<ext lowercased>"`, or the file name when it
      has no extension, or `"(no extension)"`.
5. Repo-relative path via `knot::pipeline::files::to_repo_relative`. Sort `files` by path.
6. Gitignored files never appear (the walker skips them); the report says so (§9.1 `notes`).

**Tests** (tempdir fixture):
- **Fixture:** `src/lib.rs`, `web/app.tsx`, `Makefile`, `run.sh`, `img.png` (contains a NUL),
  `big.txt` (> 500 KiB), `Cargo.lock`, a `.gitignore` that ignores `ignored.rs`, a `.git/` dir
  with a `.rs` inside, and `.knot-lite/manifest.toml`.
- **Expected classes:** one per file; `ignored.rs`, `.git/*` and `.knot-lite/*` do not appear.
- `scan_is_side_effect_free`.
- `sniff_reads_at_most_8k`: a file with a NUL only after 8 KiB, under the size limit → FilesOnly.
- **Fixture note:** `.gitignore` files are only honoured inside a git repository unless
  `.require_git(false)` is set. knot's `discover_files` does not set it, so do not set it either.
  The test runs `git init` via `std::process::Command` **inside its own tempdir only**. If `git`
  is unavailable, skip the gitignore assertion with an explicit message. Record this in Appendix D.

#### `tier.rs`
```rust
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TierRow {
    pub language: String,            // label, Appendix B
    pub files: usize,
    pub parser: bool,                // always true for Parsed rows
    pub lsp: Option<LspStatus>,      // Some(..) only for LSP-capable labels
    pub tier: u8,                    // 3 or 2
    pub graph: &'static str,         // "lsp" | "heuristic"
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StatusReport { /* see §9.1 */ }

pub fn build_report(cfg: &Config, scan: &ScanResult, index: IndexState,
                    detect: &dyn Fn(LspLang) -> LspStatus) -> StatusReport
```
**Rules**
- Parsed label with an LSP language:
  - `Found`/`Override` → tier **3**, `graph = "lsp"`,
    `note = "server found, not started (readiness is checked when queries arrive)"`.
  - `Missing`/`OverrideInvalid` → tier **2**, `graph = "heuristic"`, `note = <hint>`.
- Parsed label without an LSP language → tier **2**, `graph = "heuristic"`, `lsp = None`.
- FilesOnly → tier **1** (aggregated, not per language).
- Excluded → tier **0** (aggregated by reason).
- `detect` is called **once per `LspLang`** (memoised in a small `BTreeMap`), so the
  `typescript` and `javascript` rows share one lookup.
- **Ordering:** rows by tier desc, files desc, language asc. `files_only.top` holds at most 10
  keys, by count desc, then key asc.
- **Totals:** `files = parsed + files_only + excluded`; `tier3`/`tier2` = sums of the rows;
  `tier1 = files_only.files`; `tier0 = excluded.files`.

**Tests** (injected `detect` closure — no real PATH):
- rust found → tier 3; java missing → tier 2 with hint.
- ts and js share one detect call (counter).
- markdown → tier 2 without lsp.
- Totals add up.
- Ordering is deterministic (shuffled input → same JSON).
- `files_only.top` is capped at 10.

**Acceptance:** green; commit.

### P1.7 — `src/error.rs` and `src/cli_tools/{status,query}.rs`

#### `error.rs`
```rust
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum QueryUnavailable {
    #[error("{tool}: knot-lite has no index for repository '{repo}' ({manifest} not found). Indexing is not available in knot-lite {version} yet. Use grep or your editor's language server meanwhile; call `status` for this repository's capability matrix.")]
    NotIndexed { tool: String, repo: String, manifest: String, version: String },
    #[error("{tool}: knot-lite found an index for repository '{repo}' (schema {schema}, written by knot-lite {written_by}) but knot-lite {version} cannot query indexes yet.")]
    NotImplemented { tool: String, repo: String, schema: String, written_by: String, version: String },
    #[error("{tool}: the index of repository '{repo}' uses schema '{schema}', which knot-lite {version} does not support (expected '{expected}'). Re-index with a matching knot-lite version.")]
    Unsupported { tool: String, repo: String, schema: String, expected: String, version: String },
    #[error("{tool}: the index manifest of repository '{repo}' ({manifest}) could not be read: {error}")]
    Invalid { tool: String, repo: String, manifest: String, error: String },
}
pub const CLI_EXIT_QUERY_UNAVAILABLE: u8 = 2;
```

#### `cli_tools/query.rs`
```rust
pub fn run_search_hybrid_context(cfg: &Config, params: &SearchParams) -> Result<serde_json::Value, QueryUnavailable>
pub fn run_find_callers(cfg: &Config, params: &CallersParams) -> Result<serde_json::Value, QueryUnavailable>
pub fn run_explore_file(cfg: &Config, params: &ExploreParams) -> Result<serde_json::Value, QueryUnavailable>
pub fn run_list_files(cfg: &Config, params: &FilesParams) -> Result<serde_json::Value, QueryUnavailable>
```
- Each body is a single call, `Err(unavailable(cfg, "<tool name>"))`, which keeps dupes at 0.
  The params are named `_params` and documented as the contract for Phases 3/4.
- `fn unavailable(cfg, tool) -> QueryUnavailable` maps
  `read_index_state(&IndexLayout::for_repo(&cfg.repo_path))` to the variant, with
  `env!("CARGO_PKG_VERSION")` as `version`. It is cheap and synchronous and never scans the repo.

#### `cli_tools/status.rs`
```rust
pub fn run_status(cfg: &Config) -> anyhow::Result<serde_json::Value>     // real-env wrapper
pub fn run_status_with(cfg: &Config, detect: &dyn Fn(LspLang) -> LspStatus) -> anyhow::Result<serde_json::Value>
pub fn format_status_output(report: &serde_json::Value, output: OutputFormat) -> String
```
- `Json` → `serde_json::to_string_pretty`.
- `Table` → header lines, a comfy-table (headers `LANGUAGE`, `FILES`, `TIER`, `GRAPH`, `LSP`,
  `NOTE`; `DynamicFullWidth`; header colours as in knot's `cli_tools/repos.rs`), then the
  files-only line and the excluded line (§9.2).
- `Markdown` → the same content as GFM (used by MCP).
- The formatters read the JSON (like knot's formatters), so CLI and MCP render from one source.

**Tests**
- `run_status_with` on the P1.6 fixture with an injected detect → deterministic JSON,
  `schema == "knot-lite/status/v1"`, `index.state == "absent"`.
- `format_status_output`: the table contains `LANGUAGE` and every label; the markdown starts
  with `## knot-lite status`; the JSON round-trips.
- Each `run_*` query returns `NotIndexed` on an empty tempdir and `NotImplemented` with a v1
  manifest present (written by the test into the tempdir). The message contains the tool name,
  the repo name and `status`.
- `run_status` is side-effect free.

**Acceptance:** green; commit.

### P1.8 — `src/models/{cli_args,params}.rs`

**`params.rs`** — plain structs mirroring the tool fields minus `repo_name` (Appendix A):
`SearchParams { query, max_results: Option<i64>, kinds, path }`,
`CallersParams { entity_name, max_targets: Option<i64>, kinds }`,
`ExploreParams { file_path, include_markup: bool }`, `FilesParams { path }`;
derive `Debug, Clone, PartialEq`. Mapping constructors `from_cli(...)` (here) and
`from_tool(...)` (P1.9) are unit-tested.

**`cli_args.rs`**
```rust
#[derive(clap::Parser)]
#[command(name = "knot-lite", version, about = "Database-free code intelligence for one repository")]
pub struct Cli {
    #[command(flatten)] pub common: CommonArgs,
    #[command(subcommand)] pub command: Commands,
}

#[derive(Clone, PartialEq, Eq, clap::Subcommand)]
pub enum Commands {
    /// Show the capability matrix (tiers per language) and the index state
    Status { #[arg(short, long, value_enum, default_value_t = OutputFormat::Table)] output: OutputFormat },
    /// Search for code entities by semantic meaning (requires an index)
    Search { query: String, #[arg(short, long, default_value = "5")] max_results: usize,
             #[arg(short, long)] repo: Option<String>, #[arg(short, long)] kinds: Option<String>,
             #[arg(short, long)] path: Option<String>,
             #[arg(short, long, value_enum, default_value_t = OutputFormat::Table)] output: OutputFormat },
    /// Find all references to an entity (requires an index)
    Callers { entity_name: String, #[arg(short, long)] repo: Option<String>,
              #[arg(short = 'm', long)] max_targets: Option<usize>, #[arg(short = 'k', long)] kinds: Option<String>,
              #[arg(short, long, value_enum, default_value_t = OutputFormat::Table)] output: OutputFormat },
    /// Explore all entities in a source file (requires an index)
    Explore { file_path: String, #[arg(short, long)] repo: Option<String>, #[arg(long)] include_markup: bool,
              #[arg(short, long, value_enum, default_value_t = OutputFormat::Table)] output: OutputFormat },
    /// List the indexed files (requires an index)
    Files { #[arg(short, long)] path: Option<String>, #[arg(short, long)] repo: Option<String>,
            #[arg(short, long, value_enum, default_value_t = OutputFormat::Table)] output: OutputFormat },
}
```
- Flags, short options and defaults are identical to knot's `src/models/cli_args.rs` (read it);
  reuse knot's doc comments, adapted where knot says "indexed codebases". Note the short-flag
  clash in `Search`: knot uses `-p` for `path`; verify that knot's derive resolves it and mirror
  exactly what knot does.
- `-r/--repo` goes through the same scope check as MCP (`check_repo_scope_str`, P1.9).

**Tests:** `Cli::try_parse_from` for each subcommand with all flags; global `--repo-path`
accepted before **and** after the subcommand; defaults (`max_results == 5`, `output == Table`);
`knot-lite` without a subcommand → error; the mapping constructors.

**Acceptance:** green; commit.

### P1.9 — `src/models/repo_scope.rs` and `src/mcp_tools/*`

#### `repo_scope.rs`
```rust
/// knot-lite serves one repository. Accepts knot's repo_name forms (absent/null,
/// string with commas, "all"/"*", array of strings) and checks they only name
/// this repository.
pub fn check_repo_scope(value: Option<&serde_json::Value>, served: &str) -> Result<(), String>
pub fn check_repo_scope_str(value: Option<&str>, served: &str) -> Result<(), String>
```
- **Ok:** absent, `null` or `""`; `"all"` or `"*"` (case-insensitive, trimmed); a comma list or
  array whose trimmed, non-empty items are all `== served` or a sentinel.
- **Error text:** `repo_name '{given}' is not served here: this knot-lite server serves only '{served}'. Omit repo_name, or pass '{served}', 'all' or '*'.`
- Any non-string/non-array type → `repo_name must be a string or an array of strings`.
- **Tests:** every accepted form; a foreign name; a mixed list (`"x,served"` → error naming `x`);
  a number → error.

#### `mcp_tools/mod.rs` (shared machinery — keeps every tool file tiny)
```rust
/// Deserialize tool arguments after validating and removing `repo_name`
/// (knot accepts arrays for it; the typed struct only models the string form).
pub(crate) fn parse_args<T: serde::de::DeserializeOwned>(
    tool: &str, params: &CallToolRequestParams, served_repo: &str,
) -> Result<T, CallToolError>

/// Common body of the four query tools.
pub(crate) fn query_tool<T, P>(
    tool: &str,
    params: CallToolRequestParams,
    handler: &KnotLiteMcpHandler,
    to_params: impl FnOnce(T) -> P,
    run: impl FnOnce(&Config, &P) -> Result<serde_json::Value, QueryUnavailable>,
) -> Result<CallToolResult, CallToolError>
where T: serde::de::DeserializeOwned
```
- `parse_args`:
  1. `let mut args = params.arguments.clone().unwrap_or_default();`
  2. `check_repo_scope(args.get("repo_name"), served)`; on error →
     `CallToolError::from_message(format!("{tool}: {e}"))`.
  3. `args.remove("repo_name");`
  4. `serde_json::from_value(Value::Object(args))`; on error →
     `CallToolError::from_message(format!("invalid arguments for {tool}: {e}"))`.
- `query_tool`:
  1. Get the config (`handler.config()?`).
  2. `parse_args::<T>`.
  3. `run(cfg, &to_params(args))`.
  4. `Ok(json)` → `CallToolResult::text_content(vec![TextContent::new(pretty_json, None, None)])`
     (unreachable in Phase 1 but correct).
  5. `Err(e)` → `CallToolError::from_message(e.to_string())`.

#### Tool files
Each defines the `#[mcp_tool]` struct (derive `JsonSchema`, `serde::Deserialize`, `Debug`) with
the **exact fields and constraints of Appendix A**, plus:
```rust
impl SearchHybridContextTool {
    pub async fn handle(params: CallToolRequestParams, handler: &KnotLiteMcpHandler)
        -> Result<CallToolResult, CallToolError> {
        mcp_tools::query_tool::<Self, _>("search_hybrid_context", params, handler,
            SearchParams::from_tool, cli_tools::run_search_hybrid_context)
    }
}
```
- Use the verified `tool_name()` from P1.3 instead of the literal if it returns a `&str`/`String`
  usable here.
- `from_tool` lives in `params.rs`, next to `from_cli` (one place for mappings).

`status.rs`:
```rust
#[mcp_tool(name = "status", title = "Repository capability matrix", description = "…Appendix A…",
           read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)]
#[derive(JsonSchema, serde::Deserialize, Debug)]
pub struct StatusTool { /* repo_name only, Appendix A */ }
impl StatusTool {
    pub async fn handle(params: CallToolRequestParams, handler: &KnotLiteMcpHandler)
        -> Result<CallToolResult, CallToolError> {
        // 1. parse_args::<Self>; cfg = handler.config()?.clone()
        // 2. json = tokio::task::spawn_blocking(move || run_status(&cfg)).await
        //    (map JoinError and anyhow errors to CallToolError::from_message("status: …"))
        // 3. text = format_status_output(&json, OutputFormat::Markdown)
        // 4. CallToolResult::text_content(vec![TextContent::new(text, None, None)])
        //        .with_structured_content(json.as_object().cloned().unwrap_or_default())
    }
}
```
All tools: `read_only_hint = true`, `destructive_hint = false`, `idempotent_hint = true`,
`open_world_hint = false`.

**Tests (`mcp_tools/mod.rs`, mirroring knot's module tests)**
- `test_all_tools_have_valid_names`, `test_all_tools_have_descriptions`,
  `test_all_tools_have_input_schema`.
- **Required fields:** `search_hybrid_context` requires `query`; `find_callers` requires
  `entity_name`; `explore_file` requires `file_path`; `list_files` and `status` have no required
  field.
- `test_all_tools_have_optional_repo_name` (all five); its description contains `'all'` and
  `comma-separated`.
- **Schema fidelity:** one assertion per Appendix A row (e.g. `max_results`: `minimum 1`,
  `maximum 100`, `default 5`; `find_callers.kinds`: `maxLength 1024`).
- `parse_args_rejects_missing_required` (the message starts with
  `invalid arguments for find_callers`).
- `parse_args_accepts_repo_name_array_of_self`; `parse_args_rejects_foreign_repo`.
- `query_tool_not_indexed_message` (tempdir config, `find_callers`): the error text contains
  `has no index` and `status`.

**Acceptance:** green; commit.

### P1.10 — `src/mcp_handler.rs`

```rust
pub struct KnotLiteMcpHandler { pub config: Option<Arc<Config>>, pub dry_run: bool }

impl KnotLiteMcpHandler {
    pub fn new(config: Config) -> Self
    pub fn new_dry_run() -> Self
    /// Error "server has no repository configured" when None.
    pub(crate) fn config(&self) -> Result<&Config, CallToolError>
}

pub const DRY_RUN_MESSAGE: &str = "Server is running in dry-run mode. The repository is not opened. \
     This mode is used for protocol validation and quality checks only.";

impl KnotLiteMcpHandler {
    /// The canonical tool surface, independent of transport (see knot's mcp_handler).
    pub fn tools() -> Vec<Tool> {
        vec![StatusTool::tool(), SearchHybridContextTool::tool(), FindCallersTool::tool(),
             ExploreFileTool::tool(), ListFilesTool::tool()]
    }
    pub async fn dispatch(&self, params: CallToolRequestParams) -> Result<CallToolResult, CallToolError> {
        if self.dry_run { return Err(CallToolError::from_message(DRY_RUN_MESSAGE.to_string())); }
        match params.name.as_str() {
            "status" => StatusTool::handle(params, self).await,
            "search_hybrid_context" => SearchHybridContextTool::handle(params, self).await,
            "find_callers" => FindCallersTool::handle(params, self).await,
            "explore_file" => ExploreFileTool::handle(params, self).await,
            "list_files" => ListFilesTool::handle(params, self).await,
            _ => Err(CallToolError::unknown_tool(params.name)),
        }
    }
}

#[async_trait]
impl ServerHandler for KnotLiteMcpHandler {
    async fn handle_list_tools_request(&self, _request: Option<PaginatedRequestParams>,
        _context: &RequestContext, _runtime: Arc<dyn McpServer>) -> Result<ListToolsResult, RpcError> {
        Ok(ListToolsResult { tools: Self::tools(), cache_scope: Default::default(),
            result_type: "complete".to_string(), ttl_ms: 0, meta: None, next_cursor: None })
    }
    async fn handle_call_tool_request(&self, params: CallToolRequestParams,
        _context: &RequestContext, _runtime: Arc<dyn McpServer>) -> Result<ServerResult, CallToolError> {
        self.dispatch(params).await.map(Into::into)
    }
}

pub const SERVER_INSTRUCTIONS: &str = "knot-lite answers code-intelligence questions about ONE repository without databases.\n\
1. status — call it first: capability matrix (tiers 3/2/1/0 per language) and index state.\n\
2. search_hybrid_context — semantic search over the index.\n\
3. find_callers — reverse references (graph provenance: lsp or heuristic).\n\
4. explore_file — entities declared in one file.\n\
5. list_files — files in the index, optionally by prefix or glob.\n\
Tools 2-5 require a knot-lite index; without one they explain what is missing.\n\
repo_name is optional: this server serves a single repository; 'all', '*' or its own name are accepted.";

pub fn build_server_details() -> ServerDetails {
    ServerDetails {
        server_info: Implementation {
            name: "knot-lite".into(), version: env!("CARGO_PKG_VERSION").into(),
            title: Some("knot-lite Code Intelligence".into()),
            description: Some("Database-free semantic search and call graph for one repository (file-based vectors + language servers)".into()),
            icons: vec![], website_url: None,
        },
        capabilities: ServerCapabilities { tools: Some(ServerCapabilitiesTools { list_changed: None }), ..Default::default() },
        instructions: Some(SERVER_INSTRUCTIONS.into()),
        meta: None,
    }
}
```
**Tests** (port knot's `mcp_handler.rs` tests, adapted):
- `EXPECTED_TOOLS = ["status","search_hybrid_context","find_callers","explore_file","list_files"]`;
  `tools_returns_the_full_surface`.
- `every_tool_has_name_description_and_object_schema`.
- **Drift guards** with `source_between(...)` over `include_str!("mcp_handler.rs")`:
  - `list_tools_request_delegates_to_tools_fn`: contains `Self::tools()`; does not contain `Tool::tool()`.
  - `call_tool_request_delegates_to_dispatch`: contains `self.dispatch(`; does not contain `SearchHybridContextTool::handle`.
- `dispatch_refuses_every_tool_in_dry_run_mode`.
- `dispatch_rejects_unknown_tool` (non-dry-run handler with a tempdir config);
  `CallToolResult::from(err).is_error == Some(true)`.
- `status_dispatch_succeeds_on_tempdir`: `is_error` is not `Some(true)`;
  `structured_content["schema"] == "knot-lite/status/v1"`.
- `query_dispatch_reports_not_indexed`.
- `server_details_*`: name `knot-lite`; the version contains `.`; the instructions mention all
  five tools; the tools capability is present.

Build `CallToolRequestParams` in tests with a helper `call_params(name, args)` that sets
`meta: RequestMetaObject::new("2026-07-28", ClientCapabilities::default())`,
`input_responses: None`, `request_state: None` (fields verified in P1.3).

**Acceptance:** green; commit.

### P1.11 — Binaries (thin) and `cli_tools::run_cli`

**`cli_tools/mod.rs`**
- `pub fn run_cli(cli: Cli) -> anyhow::Result<String>`: load the config
  (`Config::load_cli(&cli.common)`), check `-r/--repo` with `check_repo_scope_str`, map the
  command to its params, call `run_*`, and format.
- `pub fn exit_code_for(e: &anyhow::Error) -> u8`: `downcast_ref::<QueryUnavailable>()` → `2`;
  otherwise `1`.
- The query commands accept the `--output` flag but do not use it in Phase 1 (documented).

**`src/bin/knot-lite.rs`**
```rust
#[tokio::main]
async fn main() -> std::process::ExitCode {
    knot_lite::utils::init_logging(knot_lite::utils::CLI_LOG_LEVEL).ok();
    let cli = <knot_lite::models::cli_args::Cli as clap::Parser>::parse();
    match knot_lite::cli_tools::run_cli(cli) {
        Ok(out) => { print!("{out}"); std::process::ExitCode::SUCCESS }
        Err(e) => { eprintln!("error: {e:#}"); std::process::ExitCode::from(knot_lite::cli_tools::exit_code_for(&e)) }
    }
}
```

**`src/bin/knot-lite-mcp.rs`**
```rust
#[derive(clap::Parser)]
#[command(name = "knot-lite-mcp", version, about = "knot-lite MCP server (stdio, MCP 2026-07-28)")]
struct McpCli {
    #[command(flatten)] common: knot_lite::config::CommonArgs,
    /// Answer protocol requests without opening the repository (quality checks).
    #[arg(long, env = "KNOT_LITE_DRY_RUN", default_value_t = false, hide = true)]
    dry_run: bool,
}

#[tokio::main]
async fn main() -> SdkResult<()> {
    knot_lite::utils::init_logging(knot_lite::utils::MCP_LOG_LEVEL).expect("logging");
    let cli = McpCli::parse();
    let handler = if cli.dry_run { KnotLiteMcpHandler::new_dry_run() } else {
        let cfg = Config::load_mcp(&cli.common, false)
            .unwrap_or_else(|e| { eprintln!("knot-lite-mcp: {e:#}"); std::process::exit(1) });
        tracing::info!(repo = %cfg.repo_name, path = %cfg.repo_path.display(), "knot-lite MCP server starting");
        KnotLiteMcpHandler::new(cfg)
    };
    let transport = StdioTransport::new(TransportOptions::default())?;
    let server = server_runtime::create_server(McpServerOptions {
        server_details: build_server_details(), transport,
        handler: handler.to_mcp_server_handler(), message_observer: None });
    if let Err(e) = server.start().await { eprintln!("{}", e.rpc_error_message().unwrap_or(&e.to_string())); }
    Ok(())
}
```
- If clippy objects to `std::process::exit` in `main`, restructure into `run() -> anyhow::Result<()>`
  plus an `ExitCode`. **Never add `#[allow]`.**
- **Nothing** is printed to stdout outside the SDK transport; logs go to stderr (P1.4).
- Use `rpc_error_message` only if P1.3 confirmed it exists; otherwise print `e`.

**Tests (lib):** `run_cli_status_json_on_tempdir`, `run_cli_callers_not_indexed_maps_to_exit_2`,
`run_cli_foreign_repo_flag_errors_with_exit_1`, `exit_code_for_plain_error_is_1`.

**Acceptance:** `cargo build --bins`; `./target/debug/knot-lite status` in the repo root prints a
table; `./target/debug/knot-lite callers foo; echo $?` → `2`; commit.

### P1.12 — `tests/mcp_stdio.rs` (stdio integration test)

**Setup**
- Spawn `env!("CARGO_BIN_EXE_knot-lite-mcp")` with:
  - args `--repo-path <tempdir>`
  - env `RUST_LOG=off`
  - env `KNOT_LITE_CONFIG_DIR=<empty tempdir>` (isolates the user's `.env`)
  - env `KNOT_LITE_LSP_RUST`, `KNOT_LITE_LSP_TS`, `KNOT_LITE_LSP_JAVA` pointing to non-executable
    files (deterministic tiers)
  - stdin/stdout piped, stderr null
- Tempdir content: `src/lib.rs`, `run.sh`.

**Requests** (newline-delimited, Appendix C):
1. `server/discover` (id 1)
2. `tools/list` (id 2)
3. `tools/call status {}` (id 3)
4. `tools/call find_callers {"entity_name":"foo"}` (id 4)
5. `tools/call find_callers {"entity_name":"foo","repo_name":"other"}` (id 5)
6. `tools/call nope {}` (id 6)

**Reading:** read stdout lines on a reader thread through a `std::sync::mpsc` channel with
`recv_timeout(60 s)`, and collect responses by `id`. Every stdout line must parse as JSON — any
non-JSON line fails the test, which guards against log leakage. Kill and wait the child at the end.

**Assertions**
- 1: `result.serverInfo.name == "knot-lite"`.
- 2: the tool-name set equals the 5 expected tools; `find_callers.inputSchema.required == ["entity_name"]`.
- 3: `result.isError` absent or false; `result.structuredContent.schema == "knot-lite/status/v1"`;
  the rust row has tier 2 (invalid override); files-only counts `.sh`.
- 4: `result.isError == true`; the text contains `has no index`.
- 5: `result.isError == true`; the text contains `not served here`.
- 6: either a JSON-RPC `error` or `isError == true`, as recorded in P1.3. Assert the recorded variant.
- Optional negative test: a request without `_meta` → the JSON-RPC `error` member is present
  (the code is recorded in Appendix D).

Fully cross-platform: no shell, no Python.

**Acceptance:** `cargo test --test mcp_stdio` green; `make check` green **and its output shows
the `tests/mcp_stdio.rs` run** (P1-D16). Commit.

### P1.13 — Docs: README, AGENTS, report note

1. **`README.md`** (root, new). Sections:
   - What knot-lite is, and how it relates to knot (same parsers, no Qdrant/Neo4j).
   - Why it works: the Phase 0 numbers (LSP recall 100/97/95 % vs knot Neo4j 66/95/54 %;
     `i8pv_b64` overlap@10 100 %). Link `docs/spikes/phase0-results.md`.
   - The capability-tier table (Phase 0 §2) with the Phase 1 caveat (tier 3 = server binary
     found, not started).
   - Project status: Phase 1 ships `status` only; the query tools answer "no index" until
     Phases 2–4.
   - Install: `make install` → `~/.cargo/bin/knot-lite{,-mcp}`.
   - CLI: `knot-lite status [-o table|json|markdown]`, the query subcommands, exit codes (§9.3).
   - Configuration: the P1.4 precedence and an env-var table (`KNOT_LITE_REPO_PATH`,
     `KNOT_LITE_REPO_NAME`, `KNOT_LITE_CONFIG_DIR`, `KNOT_LITE_LSP_{RUST,TS,JAVA}`,
     `KNOT_LITE_DRY_RUN`, `RUST_LOG`).
   - MCP setup (opencode 2), with this example:
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
     Note that the server speaks MCP **2026-07-28** (stateless) and needs a client that supports
     it (opencode 2). Verify the exact opencode 2 syntax against its documentation at execution
     time and record it in Appendix D.
   - Language servers: install hints per language; overrides.
   - Quality gates: `make check` (fmt-check, clippy, every `cargo test` target incl. the stdio
     test, dupes), `make test-lib` (fast loop), `make check CRATE_DIR=docs/spikes/phase0`, the
     dupes exclusion.
   - Rename note: formerly knot-rag; Phase 0 artefacts keep the old name.
2. **`AGENTS.md`** (root, new — P1-D15). Contents:
   - The condensed rules of §5 and the layout of §6.
   - Gate commands.
   - The "mirror knot" architecture rule: `run_*` in `cli_tools`, thin `mcp_tools`, the
     `tools()`/`dispatch()` drift guards.
   - The single-repository rule and the read-only rule for analysed repos.
   - How to add a tool: struct in `mcp_tools/`, `run_*` in `cli_tools/`, register in `tools()`
     + `dispatch()` + `EXPECTED_TOOLS`, add the CLI subcommand, extend Appendix A-style schema tests.
   - Where the plans live (`docs/PLAN_PHASE*.md`).
3. **`docs/spikes/phase0-results.md`:** add one blockquote right under the title block:
   `> **Naming (2026-10-04):** from Phase 1 on the product is called **knot-lite** (crate, binaries, `.knot-lite/` index, `KNOT_LITE_*`). This report keeps the original knot-rag naming.`
   Change nothing else. **Do not** re-run `summarize`.

**Acceptance:** every command in the README was run once (outputs recorded in Appendix D); commit.

### P1.14 — Re-measure the clippy curves (Phase 0 Appendix F.6)

**Method** (the real `clippy.toml` is not edited while measuring):
1. For each lint and candidate value, create `/tmp/knot-lite-clippy/<lint>-<value>/clippy.toml`
   with the root file's content and only that value changed, then run
   `CLIPPY_CONF_DIR=/tmp/knot-lite-clippy/<lint>-<value> cargo clippy --all-targets --all-features --message-format=short 2>&1 | grep -c "<clippy lint name>"`.
   Candidates:
   - `too-many-arguments-threshold`: 3, 4, 5
   - `type-complexity-threshold`: 100, 150, 200
   - `excessive-nesting-threshold`: 4, 5, 6
   - `too-many-lines-threshold`: 50, 80, 100
   - `cognitive-complexity-threshold`: 10, 15, 20
   If `CLIPPY_CONF_DIR` is not honoured (verify with a deliberately extreme value), fall back to
   temporarily editing `clippy.toml` and restore it from git after each run.
2. Record each curve as a comment line in `clippy.toml` under the knot curve:
   `# knot-lite measured curve (Phase 1, <date>): 3 -> n | 4 -> n | 5 -> n`.
3. **Tighten only** to the documented later targets (`type-complexity-threshold = 150`,
   `excessive-nesting-threshold = 5`), and only if the measured count at that value is **0**.
   Leave the other values unchanged (the code base is still tiny; premature tightening would
   bind Phases 2–4). Report the changes to the human.

**Acceptance:** `make check` green with the final `clippy.toml`; commit.

### P1.15 — Final gate, smoke runs and hand-off

1. From the repository root:
   - `make check` (green)
   - `make dupes-cleanup` (no stale entries)
   - `make e2e` (skip message)
   - confirm the `make check` output includes the `mcp_stdio` integration-test run
   - `make check CRATE_DIR=docs/spikes/phase0` (the spike is still green)
2. Invoke the **`validator`** subagent with working directory
   `/home/raul/workspace/rust/knot-lite`. Tell it the gate is `make check`, that `make test`
   runs every `cargo test` target (incl. one stdio integration test), and ask it to report the dupes result explicitly.
3. Read-only smoke runs with the release build (`cargo build --release`). Record outputs and wall
   times in Appendix D:
   - `knot-lite --repo-path ~/.knot/repos/knot status`
   - `knot-lite --repo-path ~/.knot/repos/knot-site status -o markdown`
   - `knot-lite --repo-path ~/.knot/repos/spring-ai status -o json | head -60`
   - `knot-lite status` (the knot-lite repo itself)
   - `knot-lite --repo-path ~/.knot/repos/knot callers resolve_reference_intents; echo $?` → `2`
   - Afterwards confirm nothing was written: `ls -a ~/.knot/repos/knot | grep -c knot-lite` → `0`.

   Expected on this machine (all three servers are installed): the rust, typescript, javascript
   and java rows at tier 3; Makefile/Dockerfile/`.sh` files under files-only. **Do not tune
   anything to match expectations; record what you see.**
4. Manual stdio smoke with the release binary: pipe the Appendix C requests into
   `knot-lite-mcp --repo-path ~/.knot/repos/knot` and paste the trimmed transcript into Appendix D.
5. Complete Appendix D: deviations, verified API facts, measurements, any `#[expect]`, any
   `.dupes-ignore.toml` entry (expected: none of either).
6. Final message to the human:
   - the DoD checklist (§11) with ticks
   - the smoke `status` table for knot
   - test counts and the dupes result
   - the clippy curve changes
   - reported exceptions
   - the commit list of `phase-1`
   **Never push or merge.**

---

## 9. Output contracts

### 9.1 `status` JSON (`schema: "knot-lite/status/v1"`)
```json
{
  "schema": "knot-lite/status/v1",
  "knot_lite_version": "0.1.0",
  "repo": { "name": "knot", "path": "/home/raul/.knot/repos/knot" },
  "index": { "state": "absent", "manifest": ".knot-lite/manifest.toml" },
  "languages": [
    { "language": "rust", "files": 250, "parser": true,
      "lsp": { "status": "found", "server": "rust-analyzer", "path": "/home/raul/.cargo/bin/rust-analyzer" },
      "tier": 3, "graph": "lsp", "note": "server found, not started (readiness is checked when queries arrive)" },
    { "language": "markdown", "files": 12, "parser": true, "lsp": null, "tier": 2, "graph": "heuristic", "note": null }
  ],
  "files_only": { "files": 9, "top": [ { "key": ".sh", "files": 5 }, { "key": "Makefile", "files": 1 } ] },
  "excluded": { "files": 2, "by_reason": { "binary": 1, "lock_file": 1, "too_large": 0 } },
  "totals": { "files": 273, "tier3": 250, "tier2": 12, "tier1": 9, "tier0": 2 },
  "notes": [
    "Gitignored files are not counted.",
    "Tier 3 means the language server binary was found; it is started on demand in later phases."
  ]
}
```
- `index` serialises `IndexState` plus a `manifest` key (repo-relative path).
- `excluded.by_reason` always contains all three keys (`BTreeMap` → stable order).
- The numbers are illustrative only.

### 9.2 Table rendering (CLI default)
```
knot-lite status — repository 'knot' (/home/raul/.knot/repos/knot)
index: absent (.knot-lite/manifest.toml not found)
┌──────────┬───────┬──────┬───────────┬──────────────────────────────┬─────────────────────────────┐
│ LANGUAGE │ FILES │ TIER │ GRAPH     │ LSP                          │ NOTE                        │
…
files-only (tier 1): 9 files — .sh 5, Makefile 1, …
excluded (tier 0): 2 files — binary 1, lock file 1, too large 0 (gitignored files are not counted)
```
The LSP cell:

| Status | Rendering |
|---|---|
| found | `rust-analyzer ✓ /path` |
| override | `rust-analyzer ✓ /path (override)` |
| missing | `rust-analyzer ✗ not found (searched 18 dirs)` |
| override invalid | `rust-analyzer ✗ override '<value>' is not executable` |
| no LSP for the language | `—` |

Markdown rendering: `## knot-lite status`, a two-line summary, the same table as GFM, then the
two summary lines as bullets.

### 9.3 CLI exit codes
| Code | Meaning |
|---|---|
| 0 | success (`status`) |
| 1 | any error (bad repository path, foreign `--repo`, I/O) — message on stderr prefixed `error:` |
| 2 | query unavailable (`QueryUnavailable`: no index / unsupported / invalid / not implemented) |

### 9.4 MCP error texts (asserted by tests; keep stable)
- Dry-run: `DRY_RUN_MESSAGE` (P1.10).
- Arguments: `invalid arguments for <tool>: <serde message>`.
- Scope: `<tool>: repo_name '<x>' is not served here: …`.
- Query: the `QueryUnavailable` `Display` texts (P1.7).

## 10. Risks and pitfalls

| Risk | Mitigation |
|---|---|
| rust-mcp-sdk 2.0 API differs from its docs/README (young release, Aug 2026) | P1.3 runs before any MCP code, against the local source; record and adapt |
| `#[json_schema]` attributes missing or renamed in 2.0 | The P1.3 probe prints the generated schema; fall back to doc-comment descriptions and record any constraint that cannot be expressed |
| Two `rust-mcp-sdk` majors in the graph (knot 1.1 + knot-lite 2.0) | Legal; macros expand against the direct dependency; only a longer first build |
| First build is slow (knot + fastembed + ort downloads onnxruntime) | Expected; never switch the gates to `--release` |
| `.gitignore` only honoured inside git repositories (knot's `discover_files` does not `require_git(false)`) | Consistent with knot; status is accurate for real repos; the fixture runs `git init` in its own tempdir |
| Windows executables (`.cmd`, `.bat`) | PATHEXT logic is tested on Linux with `windows = true` |
| The `dupes.toml` substring exclude uses `/` | The gate runs on Linux/macOS; noted in the comment |
| Logs on stdout corrupt the MCP stream | Logging writer = stderr; the stdio test fails on any non-JSON stdout line |
| Cognitive complexity inflated by tracing macros (+7 each, knot's note) | Keep handlers free of tracing macros; log in the binaries only |
| Status latency on large repos | Measured in P1.15 (spring-ai ≈ 2.5 k files); recorded, not gated |
| `CallToolRequestParams` field set differs (MRTR fields) | The test helper is written after P1.3 |
| clap short-flag clashes when mirroring knot's `Search` flags | Copy knot's exact derive and verify with `try_parse_from` tests |

## 11. Definition of done

- [ ] Directory renamed; P1.0 baseline commit on `master`; all work on `phase-1`, one commit per task; nothing pushed.
- [ ] Root `.gitignore`, `Makefile` (`BINS`), `clippy.toml`, `dupes.toml` adapted; `docs/spikes/` excluded from the root dupes gate (proof recorded); no threshold loosened.
- [ ] `Cargo.toml`: lints byte-identical to knot (diff recorded); `publish = false`; rust-mcp-sdk 2.0 with server+macros+stdio only.
- [ ] `make check` green at the root (its `test` step runs every `cargo test` target, incl. `tests/mcp_stdio.rs`); `make check CRATE_DIR=docs/spikes/phase0` green; validator green.
- [ ] `status` works in CLI (table/json/markdown) and MCP (markdown + structuredContent), is read-only, and was run on knot, knot-site, spring-ai and knot-lite (outputs in Appendix D).
- [ ] The four query tools: knot-identical schemas (one test per Appendix A row), scope check, typed `QueryUnavailable`, CLI exit code 2.
- [ ] `tools()`/`dispatch()` drift guards; dry-run refuses every tool.
- [ ] Drift test against `knot::pipeline::input::SUPPORTED_EXTENSIONS`.
- [ ] README and AGENTS.md written; naming note added to the Phase 0 report.
- [ ] clippy curves recorded; only the documented later targets tightened, and only at 0 violations.
- [ ] Appendix D complete; every `#[expect]` / `.dupes-ignore.toml` entry reported (target: none).
- [ ] No file written outside the repository (apart from `/tmp/knot-lite-*`); `~/.knot/repos/*` and `../knot` untouched.

## 12. Hand-off to Phase 2

Phase 2 may assume:
- `Config`, `IndexLayout`, `read_index_state`, `scan_repository`, the label table,
  `LspStatus`/`detect`, `QueryUnavailable` and the handler shape exist and are tested.
- **knot `master` contains the entity-identity fix** (deterministic HTML/JSX ids +
  collision-only UUID disambiguation). Status on 2026-10-04: in progress in knot; the human
  confirmed it will be merged into knot `master` before Phase 2 starts. Phase 2's first task
  re-measures it on the three snapshots (0 differing ids across two parses, 0 collisions) and
  stops if it is not met.
- Phase 2 writes `.knot-lite/manifest.toml` with `schema = "knot-lite/manifest/v1"` and
  `knot_lite_version`, and extends it (embedding model, encoding `i8pv_b64`, commit mode)
  without breaking `ManifestHeader` (unknown fields are ignored).
- Open human decision carried over: the committed-index threshold (10 % → ~25 %), which decides
  the default `commit_mode`.

---

## Appendix A — MCP tool schemas

Field names, types and constraints are **identical to knot 1.11.1** (`../knot/src/mcp_tools/*`).
Descriptions are adapted from knot's text: "indexed repositories/knot" becomes "this
repository/knot-lite", and the four query tools add the sentence
"Requires a knot-lite index; call `status` to check."

| Tool | Field | Type | Required | Constraints (`json_schema`) |
|---|---|---|---|---|
| `search_hybrid_context` | `query` | String | ✓ | min_length 1, max_length 500 |
| | `max_results` | Option<i64> | | minimum 1, maximum 100, default 5 |
| | `repo_name` | Option<String> | | min_length 1, max_length 255 |
| | `kinds` | Option<String> | | min_length 1, max_length 255 |
| | `path` | Option<String> | | min_length 1, max_length 500 |
| `find_callers` | `entity_name` | String | ✓ | min_length 1, max_length 255 |
| | `repo_name` | Option<String> | | min_length 1, max_length 255 |
| | `max_targets` | Option<i64> | | minimum 1, maximum 500, default 25 |
| | `kinds` | Option<String> | | min_length 1, max_length 1024 |
| `explore_file` | `file_path` | String | ✓ | min_length 1 |
| | `repo_name` | Option<String> | | min_length 1, max_length 255 |
| | `include_markup` | Option<bool> | | — |
| `list_files` | `path` | Option<String> | | min_length 1, max_length 500 |
| | `repo_name` | Option<String> | | min_length 1, max_length 255 |
| `status` | `repo_name` | Option<String> | | min_length 1, max_length 255 |

**`repo_name` description** (all tools): "Optional. knot-lite serves a single repository: omit
it, or pass this repository's name, 'all' or '*'. Accepts a single name, a comma-separated list
or a JSON string array for compatibility with knot."

**`status` description:** "Read-only capability matrix of the repository served by this
knot-lite instance: for each language, the file count, whether a parser exists, whether its
language server is installed, the resulting tier (3 = parser + language server, 2 = parser
only, heuristic graph, 1 = files only, 0 = excluded) and the index state. Call this FIRST.
Returns a Markdown table plus the same data as structured JSON (schema knot-lite/status/v1)."

## Appendix B — Language label table

Filename rules (checked first, exact match):

| File name | Label |
|---|---|
| `Jenkinsfile` | `jenkins` |
| `pom.xml` | `maven` |
| `Cargo.toml` | `cargo` |
| `package.json` | `npm` |
| `tsconfig.json` | `tsconfig` |
| `Directory.Packages.props` | `msbuild` |

Extension rules (lower-cased). Every `ext` in `knot::pipeline::input::SUPPORTED_EXTENSIONS` must
be covered:

| Extensions | Label | LSP language |
|---|---|---|
| `rs` | `rust` | Rust (rust-analyzer) |
| `ts`, `tsx`, `cts` | `typescript` | Ts (typescript-language-server) |
| `js`, `jsx`, `mjs`, `cjs` | `javascript` | Ts (typescript-language-server) |
| `java` | `java` | Java (jdtls) |
| `kt`, `kts` | `kotlin` | — |
| `py`, `pyi`, `pyw` | `python` | — |
| `html`, `htm` | `html` | — |
| `css` | `css` | — |
| `scss`, `sass` | `scss` | — |
| `groovy`, `gradle` | `groovy` | — |
| `c`, `h`, `cpp`, `hpp`, `cc`, `cxx`, `hh`, `hxx` | `c_cpp` | — |
| `cs` | `csharp` | — |
| `csproj` | `msbuild` | — |
| `md` | `markdown` | — |
| `vcl`, `vtc`, `vcc` | `varnish` | — |
| `yml`, `yaml` | `yaml` | — |
| `json` | `json` | — |
| `properties` | `properties` | — |
| `tpl` | `helm` | — |

The config extensions (`yml`, `yaml`, `json`, `properties`, `tpl`) are *parsed* only when knot's
`include_config_files` is on. Phase 1 calls `discover_files(root, false)`, so most of them land
in files-only; the label table still covers them because the drift test requires it.

## Appendix C — MCP stdio wire examples (2026-07-28)

Newline-delimited JSON-RPC 2.0 on stdin; responses on stdout. Every request carries `_meta`
(verify exact acceptance in P1.3):
```json
{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}
{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"status","arguments":{},"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}
{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"find_callers","arguments":{"entity_name":"foo"},"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}
```
**Expected results**
1. `result.serverInfo.name == "knot-lite"`, and the instructions are present.
2. 5 tools.
3. `isError` absent; `structuredContent.schema == "knot-lite/status/v1"`.
4. `isError: true` with the `NotIndexed` text.

**Manual run:** `printf '%s\n' '<req1>' '<req2>' '<req3>' '<req4>' | ./target/release/knot-lite-mcp --repo-path ~/.knot/repos/knot`.

## Appendix D — Execution notes

*(Filled in by the executing agents: deviations with reasons, verified SDK signatures (P1.3),
the dupes exclusion proof (P1.1), the lint-table diff (P1.2), the opencode 2 MCP config syntax
(P1.13), clippy curves (P1.14), smoke outputs and timings (P1.15), the stdio transcript, and the
exceptions reported.)*

- Deviation (Phase 0 carry-over): the root `.gitignore` announced by Phase 0 T0.1 did not exist; created in P1.0.
- Pre-Phase-1 change (2026-10-04, human decision P1-D16): root `Makefile` `test` target changed from
  `cargo test --lib --all-features` to `cargo test --all-features`, plus a new non-gate `test-lib`
  target. Verified: `make check` green on the spike crate (89 lib tests, 3 binaries, doctests, dupes 0).

### P1.0

- The `.gitignore` deviation above is the Phase 0 carry-over (content as specified in P1.0).
- Commit on `master`: `f0e10c8 Phase 0: feasibility spikes (S1 LSP graph, S2 file-based vectors) and results`
  (172 files). Branch `phase-1` created; `git status` clean.
- `git status --short` before the commit listed 0 entries under `target/`, `.knot/`, `.idea/`, `results/raw/`.

### P1.1

- `grep -rn "knot-rag" Makefile clippy.toml dupes.toml` → no matches.
- `cargo dupes --path . report`: `Total code units analyzed: 0`, no path containing `docs/spikes/`.
  Proof the exclusion works: `cargo dupes --path docs/spikes/phase0 report` analyses 211 units
  (the same tree the root gate must skip).
- `make check CRATE_DIR=docs/spikes/phase0` green after the rename rebuild: 89 lib tests,
  3 binaries, doctests, dupes 0 groups / 6,037 lines.
- **Deviation (plan bug, fixed conservatively per ground rule 8):** the planned
  `exclude = ["tests/", "benches/", "tests.rs", "docs/spikes/"]` broke the spike's own gate.
  cargo-dupes 0.2.1 `is_excluded` (`dupes-core-0.2.1/src/scanner.rs`) is a plain substring match
  on the full walked path, and the spike reads the SAME `dupes.toml` through its
  `docs/spikes/phase0/dupes.toml -> ../../../dupes.toml` symlink. A bare `docs/spikes/` matches
  the spike's own walked paths (`docs/spikes/phase0/...`) and emptied its scan
  (`Error: No source files found in docs/spikes/phase0`, make target `dupes` exit 2).
  Fix: `exclude = ["tests/", "benches/", "tests.rs", "/docs/spikes/"]` (leading slash).
  Verified matrix (probe in `/tmp/opencode/dupes-probe`, identical files under `src/` and
  `docs/spikes/phase0/src/`):

  | pattern | `--path .` (root gate) | `--path docs/spikes/phase0` (spike gate) |
  |---|---|---|
  | `docs/spikes/` | 1 unit (spike excluded) | `No source files found` (BROKEN) |
  | `/docs/spikes/` | 1 unit (spike excluded) | 1 unit (analysed) — OK |

  Also verified `/docs/spikes/` excludes for `--path .`, `--path ./` and `--path <abs>`.
  The plan-provided comment is kept; two NOTE lines document the leading slash.
  No threshold changed; `tests/`, `benches/`, `tests.rs` kept as-is.

### P1.2

- Lint-table byte-identity check (diff empty):
  `awk '/^\[lints\.rust\]/,/allow_attributes_without_reason/' ../knot/Cargo.toml > /tmp/knot-lite-notes/lints-knot.txt`
  `awk '/^\[lints\.rust\]/,/allow_attributes_without_reason/' Cargo.toml > /tmp/knot-lite-notes/lints-knot-lite.txt`
  `diff /tmp/knot-lite-notes/lints-knot.txt /tmp/knot-lite-notes/lints-knot-lite.txt` → no output.
- `cargo build` green in 55.5 s (knot 1.11.1 + fastembed/ort/qdrant/neo4rs already cached).
- `make check` green with `CRATE_DIR` auto-detected as `.` (fmt-check, clippy, test incl. both
  bin targets + doctests, dupes 0).

### P1.3 — verified rust-mcp-sdk 2.0.0 / rust-mcp-schema 2.0.0 / rust-mcp-macros 2.0.0 facts

Sources: `~/.cargo/registry/src/index.crates.io-*/rust-mcp-{sdk,schema,macros,transport}-2.0.0`.
Probe crate: `/tmp/knot-lite-sdk-probe` (throw-away, not committed); it compiles the P1.10 handler
shape and prints the generated tool schema.

**Module paths (deviation from the plan's §4 sketch — use these):**

| Item | Path |
|---|---|
| `ServerHandler`, `McpServerOptions`, `server_runtime`, `ToMcpServerHandler` | `rust_mcp_sdk::mcp_server::*` |
| `McpServer`, `RequestContext`, `ServerDetails`, `StdioTransport`, `TransportOptions`, `SdkResult`/`McpSdkError` | `rust_mcp_sdk::*` (root re-exports of `mcp_traits` / `rust_mcp_transport`; `McpSdkError` also at `rust_mcp_sdk::error`) |
| schema types (`CallToolRequestParams`, `CallToolResult`, `Tool`, `ListToolsResult`, …) and `CallToolError` | `rust_mcp_sdk::schema::*` (there is no direct `rust_mcp_schema` dependency) |
| `#[mcp_tool]`, `#[derive(JsonSchema)]` | `rust_mcp_sdk::macros::*` (there is no direct `rust_mcp_macros` dependency) |

- `McpServerOptions<T>` (generic over transport) fields: `server_details: ServerDetails`,
  `transport: T`, `handler: Arc<dyn McpServerHandler>`,
  `message_observer: Option<Arc<dyn McpObserver<ClientMessage, ServerMessage>>>`.
  `server_runtime::create_server<T>(options) -> Arc<ServerRuntime>`;
  `ServerRuntime::start(self: Arc<Self>) -> SdkResult<()>`.
- `ServerHandler` (via `#[async_trait]`):
  `handle_list_tools_request(&self, params: Option<PaginatedRequestParams>, context: &RequestContext, runtime: Arc<dyn McpServer>) -> Result<ListToolsResult, RpcError>`;
  `handle_call_tool_request(&self, params: CallToolRequestParams, context: &RequestContext, runtime: Arc<dyn McpServer>) -> Result<ServerResult, CallToolError>`.
  Both match the plan. There is also `handle_call_tool_request_mrtr` (SEP-2322); the runtime calls
  the MRTR variant whose default delegates to `handle_call_tool_request` and converts
  `CallToolError` into a `CallToolResult` with `is_error: Some(true)`.
- `ServerDetails { server_info: Implementation, capabilities: ServerCapabilities,
  instructions: Option<String>, meta: Option<ResultMetaObject> }` (no `protocol_version`).
  `Implementation { description, icons: Vec<Icon>, name, title, version, website_url }` (Default).
  `ServerCapabilities { completions, experimental, extensions, logging, prompts, resources,
  tools: Option<ServerCapabilitiesTools> }` (Default);
  `ServerCapabilitiesTools { list_changed: Option<bool> }` (Default).
- `ListToolsResult { cache_scope, meta, next_cursor, result_type: String, tools: Vec<Tool>,
  ttl_ms: u64 }`.
  **Deviation:** `cache_scope` is `ListToolsResultCacheScope` (`Private` | `Public`) and has **no
  `Default`**, so the plan's `cache_scope: Default::default()` does not compile. knot-lite uses
  `ListToolsResultCacheScope::Public` (tool lists are not user-specific).
- `CallToolError(pub Box<dyn Error>)` — constructors `new(E)`, `unknown_tool(String)`,
  `from_message(String)`, `invalid_arguments(tool, Option<String>)`
  (formats `Invalid arguments for tool '<tool>': <msg>` — NOT the plan's wording; knot-lite keeps
  its own `invalid arguments for <tool>: <e>` wording via `from_message` per §9.4).
  `Display` writes the inner error. `From<CallToolError> for CallToolResult` sets
  `is_error: Some(true)`, `result_type: "complete"`; `From<CallToolError> for RpcError` maps to
  `internal_error` with the message.
- `CallToolResult { content, is_error: Option<bool>, meta, result_type: String,
  structured_content: Option<Value> }`; ctors `text_content(Vec<TextContent>)`,
  `with_structured_content(serde_json::Map<String, Value>)`, `with_error(CallToolError)`.
  `TextContent::new(text: String, annotations: Option<Annotations>, meta: Option<MetaObject>)`
  (3 args, matches the plan).
- `CallToolRequestParams { arguments: Option<Map>, input_responses: Option<InputResponses>,
  meta: RequestMetaObject, name: String, request_state: Option<String> }` — `meta` is required
  (wire `_meta`). `RequestMetaObject::new(protocol_version: impl Into<String>, client_capabilities:
  ClientCapabilities)`; wire keys `io.modelcontextprotocol/protocolVersion` and
  `io.modelcontextprotocol/clientCapabilities` both required. `CallToolRequestParams::new(name, meta)`
  and `.with_arguments(map)` builders exist (useful for tests).
- `#[mcp_tool]` (2.0, `execution` attribute removed) generates `tool_name() -> String`,
  `request_params() -> CallToolRequestParams`, `tool() -> Tool`. `#[derive(JsonSchema)]`
  (from `rust_mcp_sdk::macros`) generates `json_schema() -> serde_json::Map<String, Value>`;
  field attributes `#[json_schema(description, min_length, max_length, minimum, maximum, default)]`
  all supported (verified in the probe output below). `Option<T>` is not required and widens the
  type to `["X", "null"]`. `JsonSchema` + `serde::Deserialize` derive together (verified).
- Stdio framing: newline-delimited JSON on stdin/stdout;
  `DEFAULT_MAX_LINE_LENGTH = 16 * 1024 * 1024`; `TransportOptions::default()`
  (`timeout`, `max_line_length`, `channel_capacity`).
- `McpSdkError::rpc_error_message(&self) -> Option<&String>` exists (use it in the MCP binary).

**Probe output (trimmed)** — `Probe { name: String (min 1, max 10), count: Option<i64>
(minimum 1, maximum 100, default 5), flag: Option<bool> }`:

```json
"inputSchema": {
  "type": "object",
  "properties": {
    "count": { "default": 5, "description": "A count", "maximum": 100, "minimum": 1, "type": ["integer", "null"] },
    "flag":  { "description": "Optional flag", "type": ["boolean", "null"] },
    "name":  { "description": "A name", "maxLength": 10, "minLength": 1, "type": "string" }
  },
  "required": ["name"]
}
```
`Probe::tool_name()` → `"probe"` (a `String`). Annotations serialize as
`destructiveHint/idempotentHint/openWorldHint/readOnlyHint`.

**Wire behaviour (live probe, `--serve` + piped requests) — decisions for P1.12:**

1. `server/discover` (with `_meta`) → JSON-RPC result with `capabilities`, `instructions`,
   `supportedVersions: ["2026-07-28"]`, `cacheScope`, `ttlMs`, `resultType`.
   **Deviation:** `DiscoverResult` has **no top-level `serverInfo` field**; the server identity is
   stamped into `result._meta["io.modelcontextprotocol/serverInfo"]`
   (name/version/title/description). P1.12 assertion 1 must read
   `result._meta["io.modelcontextprotocol/serverInfo"].name == "knot-lite"`.
2. `tools/list` → `result.tools` as expected; `cacheScope` serializes the chosen enum variant
   (`"public"`), `ttlMs: 0`, `resultType: "complete"`.
3. `tools/call` of an **unknown tool** (valid `_meta`) → JSON-RPC **result** with
   `isError: true` and content text `Unknown tool: <name>` (NOT a JSON-RPC error). P1.12
   assertion 6 asserts the `isError` variant.
4. A request **without `_meta`** (or without `protocolVersion`) does **not** produce the planned
   "JSON-RPC `error` member with a recorded code" and is not dropped: the typed request fails to
   deserialize and the message falls through to the SDK's `CustomRequest` variant, answered with
   **`error.code = -32601`, `message = "No handler is implemented for custom requests."`**.
   The optional P1.12 negative test asserts that (deviation from §8/P1.12).
   (Truly malformed JSON *is* silently dropped by the transport — `mcp_stream.rs` continues —
   but any well-formed JSON-RPC object with a `method` gets an answer.)
5. Responses are emitted in arrival order (fast failures answered first) — P1.12 must collect by
   `id`, as planned.
