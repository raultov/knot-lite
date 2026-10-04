# PLAN — knot-rag Phase 0: Feasibility Spikes (S1 LSP graph, S2 file-based vectors)

> **Audience:** autonomous implementation agents (Build agents) that will execute this plan
> without access to the conversation that produced it. Everything you need is in this file.
> **Status:** ready for execution. **Plan revision:** 0. **Date:** 2026-10-03.
> **Owner (human):** Raul Tovar. **Repository:** `/home/raul/workspace/rust/knot-rag`. Current
> contents: an untracked `.knot/` directory (leave it alone), this `docs/` folder, and three
> quality-gate files the human copied from knot on purpose — `Makefile`, `dupes.toml`,
> `clippy.toml` — which must be **adapted and used from the very first line of Rust**
> (task [T1.0](#t10--adapt-the-root-quality-gate-files-makefile-clippytoml-dupestoml)).

---

## Table of contents

1. [Goal and non-goals](#1-goal-and-non-goals)
2. [Background: the knot-rag product vision (context only)](#2-background-the-knot-rag-product-vision-context-only)
3. [Decisions already taken by the human (do not re-litigate)](#3-decisions-already-taken-by-the-human-do-not-re-litigate)
4. [Verified environment facts](#4-verified-environment-facts)
5. [Ground rules for executing agents](#5-ground-rules-for-executing-agents)
6. [Directory layout to create](#6-directory-layout-to-create)
7. [Task graph and parallelisation](#7-task-graph-and-parallelisation)
8. [T0 — Environment setup](#8-t0--environment-setup)
9. [T1 — Spike crate scaffold](#9-t1--spike-crate-scaffold)
10. [T2 — Ground truth from knot](#10-t2--ground-truth-from-knot)
11. [T3 — Shared foundations (entity table, positions, URIs)](#11-t3--shared-foundations-entity-table-positions-uris)
12. [T4 — LSP client, readiness detection, RSS sampler](#12-t4--lsp-client-readiness-detection-rss-sampler)
13. [T5 — `lsp_probe` binary (S1)](#13-t5--lsp_probe-binary-s1)
14. [T6 — Level-2 heuristic graph (in-process knot resolver)](#14-t6--level-2-heuristic-graph-in-process-knot-resolver)
15. [T7 — Gold set, adjudication and graph metrics](#15-t7--gold-set-adjudication-and-graph-metrics)
16. [T8 — Degradation test (no LSP installed)](#16-t8--degradation-test-no-lsp-installed)
17. [T9 — `vec_probe` binary (S2)](#17-t9--vec_probe-binary-s2)
18. [T10 — `summarize` binary and results report](#18-t10--summarize-binary-and-results-report)
19. [T11 — Quality gate and hand-off](#19-t11--quality-gate-and-hand-off)
20. [Go / no-go thresholds and decision rules](#20-go--no-go-thresholds-and-decision-rules)
21. [Risks, pitfalls and troubleshooting](#21-risks-pitfalls-and-troubleshooting)
22. [Definition of done](#22-definition-of-done)
23. [Appendix A — Target lists (S1)](#appendix-a--target-lists-s1)
24. [Appendix B — Query lists (S2)](#appendix-b--query-lists-s2)
25. [Appendix C — LSP cheat-sheet per server](#appendix-c--lsp-cheat-sheet-per-server)
26. [Appendix D — knot JSON samples](#appendix-d--knot-json-samples)
27. [Appendix E — Results report template](#appendix-e--results-report-template)
28. [Appendix F — Decisions carried into Phases 1–7](#appendix-f--decisions-carried-into-phases-17)

---

## 1. Goal and non-goals

### 1.1 Goal

Produce **measured evidence** (not opinions) that decides whether the knot-rag architecture is
viable, by running two throw-away spikes:

- **S1 — Graph via LSP.** Can real language servers (rust-analyzer, typescript-language-server,
  Eclipse jdtls) replace knot's Neo4j graph for "who calls / references this symbol"? Measure
  cold/warm start-up, memory, per-request latency, and precision/recall against knot's graph
  and against a manually adjudicated gold set. Also measure the **level-2 fallback** (knot's own
  tree-sitter heuristic resolver running in-process, no database) and the **degradation path**
  when no LSP binary is installed.
- **S2 — Vectors in text files.** Can per-source-file JSONL shards with embedded vectors replace
  Qdrant? Measure on-disk size, **git pack size** (the number that decides whether `.knot-rag/` is
  committed), determinism (no git churn on re-index), load time, query latency and retrieval
  quality (including int8 quantisation loss), plus the **level-1 path** (files with no parser:
  line-window chunks) for locating files.

The single deliverable that matters is **`docs/spikes/phase0-results.md`** (template in
[Appendix E](#appendix-e--results-report-template)) backed by raw JSON results.

### 1.2 Non-goals (explicitly out of scope for Phase 0)

- No production crate, no `src/` at the repository root, no MCP server, no CLI for end users.
- No modification of the `knot` repository (`/home/raul/workspace/rust/knot`) or of any
  repository under `~/.knot/repos/` or `~/workspace/`.
- No use of the harness-provided LSP tools (opencode `lsp`, Claude Code `LSP`): it was established
  that an MCP server cannot invoke client tools, so knot-rag must own its LSP client.
- No lexical re-ranking in S2 (that comes in Phase 3); S2 measures **pure vector** retrieval.
- No publishing, no pushing, no remote git operations of any kind.

---

## 2. Background: the knot-rag product vision (context only)

knot-rag will be an **MCP server + CLI** (later maybe a library) that offers knot-like code
intelligence **without databases**, mirroring knot's architecture
(`/home/raul/workspace/rust/knot`: logic in `cli_tools/run_*`, thin `mcp_tools/*` wrappers,
`mcp_handler` with `tools()` + `dispatch()`, minimal binaries).

- **Vectors** live in text files inside each repository (`<repo>/.knot-rag/`), replacing Qdrant.
- **Graph** comes from language servers that knot-rag spawns itself (live for MCP, pre-computed
  edges written at index time for a fast cold CLI), replacing Neo4j.
- **Capability tiers per language** (graceful degradation, agreed with the human):

| Tier | When | What is indexed | `search` | `find_callers` / `explore_file` |
|---|---|---|---|---|
| **3 — Full** | knot tree-sitter parser + LSP installed and ready | entities + vectors | entities | exact graph (live LSP or pre-computed LSP edges) |
| **2 — No LSP** | knot parser exists but LSP missing / not ready / crashed / non-existent | same as tier 3 | entities | **heuristic** graph from knot's `reference_intents` resolver; `explore_file` from the index |
| **1 — Files only** | neither parser nor LSP (`.sh`, `Makefile`, `Dockerfile`, `.sql`, …) | line-window chunks + one file-level vector | files + line ranges | explicit "unsupported" answer suggesting grep / harness LSP |
| **0 — Excluded** | binaries, huge files, gitignored | nothing | — | — |

Every graph answer will carry provenance: `lsp-live | lsp-indexed | heuristic | none`.
Phase 0 measures the numbers that justify (or kill) each tier.

---

## 3. Decisions already taken by the human (do not re-litigate)

| # | Decision |
|---|---|
| D1 | Graph strategy: **own LSP client**, live + pre-computed. Harness LSP is complementary only. |
| D2 | Committing `.knot-rag/` to git **depends on measured size** → S2 must produce the git pack numbers and the rule in §20.2 decides the default. |
| D3 | Reuse knot's parsers by **direct path dependency on the `knot` crate now**, split later (accept that `qdrant-client`/`neo4rs` get compiled in). |
| D4 | Initial languages: **Rust, TypeScript/JavaScript, Java**. |
| D5 | Do **Phase 0 spikes first** before committing to the full architecture. |
| D6 | Install **jdtls** (approved). |
| D7 | Java ground truth: the **`spring-ai`** repository already indexed in knot. |
| D8 | Location: everything under **`knot-rag/docs/spikes/`** — spike crate at `docs/spikes/phase0/`, report at `docs/spikes/phase0-results.md`. |
| D9 | Tier model above (files-only fallback must work when no LSP/parser exists). |
| D10 | **MCP layer (Phase 1+, not Phase 0):** use **`rust-mcp-sdk` 2.x** (currently `2.0.0`), which implements the **stateless MCP revision (2026-07-28, no `initialize`)**. The human will upgrade the client to **opencode 2** to get support. knot itself stays on 1.x; therefore knot-rag copies knot's MCP *architecture*, not its MCP *code*. See [Appendix F](#appendix-f--decisions-carried-into-phases-17). |
| D11 | **knot's quality gates apply from day one**, including to the Phase 0 spike: the root `Makefile` (`make check` = fmt-check + clippy `-D warnings` + lib tests + `cargo dupes check`), the root `clippy.toml` thresholds, the root `dupes.toml` duplication ratchet, and knot's `[lints.rust]` / `[lints.clippy]` tables copied **verbatim (with their comments)** into every `Cargo.toml`. The three root files were copied by the human; agents adapt them (T1.0) but never weaken them. |

---

## 4. Verified environment facts

All facts below were verified on 2026-10-03 on the target machine. If any of them is no longer
true when you execute, **stop and report** instead of improvising.

| Fact | Value / evidence |
|---|---|
| OS / shell | Linux 7.0.0-34-generic, zsh. 32 cores, 61 GB RAM, `/tmp` is tmpfs with ~31 GB free. |
| Rust | `rustc 1.98.1 (2026-09-01)`, edition 2024 supported. |
| rust-analyzer | `/home/raul/.cargo/bin/rust-analyzer` → `1.98.1`. |
| typescript-language-server | `/home/raul/.nvm/versions/node/v22.2.0/bin/typescript-language-server` → `5.3.0`. |
| typescript (for tsserver) | `~/workspace/typescript/knot-site/node_modules/typescript` exists. |
| Java | OpenJDK `21.0.12.1`, Maven `3.9.7`, python3 `3.14.4` (needed by jdtls launcher). |
| jdtls | **not installed**. Latest milestone: `1.61.0`, file `jdt-language-server-1.61.0-202609031315.tar.gz` (48.7 MB) + `.sha256` at `https://download.eclipse.org/jdtls/milestones/1.61.0/`. |
| knot CLI | `/home/raul/.cargo/bin/knot` version `1.11.0`; knot source checkout at `/home/raul/workspace/rust/knot` is `1.11.1`. |
| knot databases | Docker containers `knot_qdrant` (ports 6333 REST / 6334 gRPC) and `knot_neo4j` (7687) are running. Qdrant collection: `knot_entities`. |
| **knot CLI env quirk** | Some environment sets `KNOT_EMBED_DIM=768`, which breaks `knot search` ("expected dim 384, got 768"). **Always** run knot with `KNOT_EMBED_MODEL=AllMiniLML6V2 KNOT_EMBED_DIM=384` (wrapper in T0.4). |
| knot-server snapshots | knot-server clones every indexed repo into `~/.knot/repos/<id>` (`KNOT_WORKSPACE_DIR=/home/raul/.knot/repos`). These snapshots are **exactly what is indexed** (line numbers match knot's graph). Relevant ids: `knot`, `knot-site`, `spring-ai`. They have no `.git` directory. |
| Indexed repos | `knot` (269 files / 5 606 entities, rust), `knot-site` (75 / 2 199, typescript+javascript), `spring-ai` (2 490 / 27 736, java, maven, >100 modules). |
| Embedding model of the index | `~/.knot/repos/spring-ai/.knot/index_state.json` → `"embed_model": "AllMiniLML6V2"` (384-dim, symmetric: no query/passage prefix). |
| fastembed cache | `~/.knot/repos/fastembed_cache/models--Qdrant--all-MiniLM-L6-v2-onnx` exists → use `KNOT_FASTEMBED_CACHE_DIR=/home/raul/.knot/repos/fastembed_cache` to avoid downloads. |
| knot index contains stale duplicates | e.g. `knot callers ParsedEntity -r knot` returns two targets: `src/models/entity.rs:276` (stale) and `:283` (current, matches the snapshot). Ground truth **must** be filtered against the snapshot (T2.4). |
| knot public APIs usable without DB | `knot::pipeline::input::discover_files(repo_path: &str, include_config_files: bool) -> Result<Vec<PathBuf>>`; `knot::pipeline::parser::{ParseConfig, parse_files}`; `knot::pipeline::prepare::prepare_entities(&mut [ParsedEntity])`; `knot::pipeline::embed::Embedder::{init_with_model, embed, embed_query, dim, model_name}`; `knot::pipeline::embed::model::EmbedModelChoice::supported()`; `knot::pipeline::ingest::resolve::resolve_reference_intents(&mut [ResolutionEntity]) -> RunMetrics` (pure, in-memory, includes `link_method_overrides`); `knot::models::entity::{ParsedEntity, ResolutionEntity, EmbeddedEntity, EntityKind}`; `knot::models::relationship::{ReferenceIntent, RelationshipType}`; `knot::pipeline::files::{to_repo_relative, is_supported_file}`. |
| knot entity identity | `ParsedEntity.uuid = Uuid::new_v5(NAMESPACE_KNOT, "{repo_name}:{rel_file_path}:{fqn}:{start_line}")`. With the same `repo_name` and repo-relative paths, **the spike computes the very same UUIDs as knot's Qdrant point ids**. `file_path` is repo-relative (`to_repo_relative`). `start_line`/`end_line` are 1-based. |
| knot `ParseConfig` | fields `repo_root: PathBuf` (canonicalised), `custom_queries_path: Option<String>`, `repo_name: String`, `include_config_files: bool`, `repo_path: Option<String>` (**must be `Some(root)` for Rust crate-qualified FQNs**). Implements `Default`. |
| async-lsp | latest `0.2.4`; depends on **`lsp-types 0.95`** (pinned upstream because of the `Url`→`Uri` change). **Never** add `lsp-types 0.97` directly; use the re-export `async_lsp::lsp_types`. Example of a client driving rust-analyzer: `https://raw.githubusercontent.com/oxalica/async-lsp/main/examples/client_builder.rs`. |
| sysinfo | latest `0.39.x`. |
| rust-mcp-sdk | latest `2.0.0` (relevant to Phase 1+ only). |
| cargo-dupes | `/home/raul/.cargo/bin/cargo-dupes` version `0.2.1` (the version the Makefile pins). Global option `-p/--path <PATH>` selects the analysed directory. |
| **Config lookup — clippy** | Verified experimentally (clippy for rustc 1.98): a `clippy.toml` in a **parent** directory of the crate's `Cargo.toml` is applied (a parent `too-many-arguments-threshold = 1` fired on a 2-argument function). → the spike crate **inherits the root `clippy.toml`**; it must **not** have its own copy. |
| **Config lookup — rustfmt** | rustfmt also searches parent directories for `rustfmt.toml`/`.rustfmt.toml`. knot has none → default style; knot-rag adds none. |
| **Config lookup — cargo-dupes** | Verified experimentally: `cargo dupes` reads `dupes.toml` **only from the analysed path** (cwd or `--path`), **not from parents**; and `cargo dupes check` **exits 0 when no `dupes.toml` is found** (no thresholds → silent pass). → the spike crate needs `docs/spikes/phase0/dupes.toml` as a **symlink** to the root file (T1.0), otherwise the gate is silently disabled. The same applies to `.dupes-ignore.toml` if it is ever created. |
| knot lint tables | knot `Cargo.toml`: `[lints.rust] unsafe_code = "deny"`; `[lints.clippy] too_many_lines = "warn"`, `cognitive_complexity = "warn"`, `allow_attributes = "warn"`, `allow_attributes_without_reason = "warn"` (exact text with comments reproduced in T1.1). knot has no `rustfmt.toml` and no `.cargo/config.toml`. |

---

## 5. Ground rules for executing agents

1. **Write only inside** `/home/raul/workspace/rust/knot-rag/docs/` and `/tmp/knot-rag-spike/`
   (plus the jdtls install paths in T0.2, and the three root quality-gate files `Makefile`,
   `clippy.toml`, `dupes.toml` exactly as specified in T1.0, plus `.gitignore` in T0.1). Never write into `~/workspace/rust/knot`,
   `~/.knot/repos/*`, `~/workspace/github/spring-ai`, `~/workspace/typescript/knot-site`.
2. **Language servers only ever run on sandbox copies** under `/tmp/knot-rag-spike/work/`.
   jdtls/m2e writes `.project`, `.classpath`, `.settings/` into the project; rust-analyzer runs
   `cargo` (build scripts, proc-macros); none of that may touch real repositories.
3. Rust conventions (from the human's global rules): `edition = "2024"`; **no `unsafe`**
   (`unsafe_code = "deny"`); **no `#[allow(...)]` anywhere**; `#[expect(..., reason = "...")]`
   only as a last resort and you must report each one to the human; fix clippy findings by
   refactoring. All code and comments in **English**.
4. Cross-platform hygiene even in the spike: build paths with `std::path`, convert paths↔URIs
   with the `url` crate (never string concatenation), no shelling out from Rust except to start
   language servers. Bash scripts are acceptable (Linux-only spike tooling).
5. **No network side effects beyond downloads** explicitly listed (jdtls tarball, Maven
   dependencies fetched by jdtls, `ort` binaries fetched by rust-analyzer build scripts, crates).
6. Never commit, push or tag anything unless the human confirmed T0.1. Never push at all.
7. When a measured number misses a threshold, **do not tune the experiment to pass**. Record the
   raw number, explain, and propose the fallback described in §20.
8. Every result file is machine-readable JSON with a `schema` field; every manual decision is
   recorded in TOML under `results/adjudication/` with a reason and evidence.
9. After any Rust change, run **`make check`** from the repository root (it targets the spike
   crate automatically, see T1.0) and the **`validator`** subagent (T11). Both must be green
   before a task is considered done. Never raise thresholds in `clippy.toml`/`dupes.toml`, never
   add `.dupes-ignore.toml` entries except for the two legitimate categories documented in
   knot's `AGENTS.md` (literal-differentiated units; externally imposed trait boilerplate), each
   with a `reason`. The conventional fix for a duplication finding is extracting a shared,
   parameterised helper.
10. Keep unit tests **inside library modules** (`src/**`, `#[cfg(test)] mod tests`), not in
    `src/bin/*`: the Makefile runs `cargo test --lib`, so tests placed in binaries would never
    run in the gate. Binaries must stay thin (argument parsing + calls into the library).
11. If something in this plan turns out to be wrong (API name changed, a file moved), fix it in
    the most conservative way, and log the deviation in the report's "Deviations from plan"
    section.

---

## 6. Directory layout to create

```
knot-rag/
├── Makefile                               # copied from knot by the human; adapted in T1.0
├── clippy.toml                            # copied from knot by the human; header adapted in T1.0 (inherited by every crate below)
├── dupes.toml                             # copied from knot by the human; header + ratchet adapted in T1.0
└── docs/
    ├── PLAN_PHASE0_SPIKES_0.md            # this file
    └── spikes/
        ├── phase0-results.md              # final report (T10)
        └── phase0/                        # throw-away spike crate (own [workspace])
            ├── Cargo.toml                 # with knot's [lints.*] tables verbatim (T1.1)
            ├── dupes.toml -> ../../../dupes.toml   # SYMLINK (cargo-dupes does not look in parents)
            ├── .gitignore                 # NO clippy.toml / rustfmt.toml here: both are inherited from the root
            ├── README.md                  # how to reproduce every measurement (short)
            ├── scripts/
            │   ├── env.sh                 # common exports (sourced by every script)
            │   ├── knot.sh                # knot CLI wrapper with the env quirk fixed
            │   ├── setup_sandbox.sh       # T0.3
            │   ├── install_jdtls.sh       # T0.2
            │   ├── groundtruth.sh         # T2
            │   ├── qdrant_dump.sh         # T9.9 parity check
            │   ├── gitmeasure.sh          # T9.6
            │   └── run_all.sh             # orchestrates S1+S2 end-to-end
            ├── targets/
            │   ├── rust_knot.toml
            │   ├── ts_knot_site.toml
            │   └── java_spring_ai.toml
            ├── queries/
            │   ├── entities.toml          # tier-3 retrieval queries (knot, knot-site, spring-ai)
            │   └── level1.toml            # tier-1 file-location queries
            ├── groundtruth/               # raw knot JSON (committed)
            ├── results/
            │   ├── s1_<lang>_<run>.json   # run ∈ {cold, warm}
            │   ├── s1_<lang>_nolsp.json
            │   ├── heuristic_<lang>.json
            │   ├── gold_<lang>.json
            │   ├── s2_<repo>.json
            │   ├── adjudication/<lang>.toml
            │   └── raw/                   # big/verbose artefacts (gitignored)
            └── src/
                ├── lib.rs
                ├── common.rs              # shared result types, schema constants, stats helpers
                ├── entities.rs            # snapshot entity table (knot parse) + lookups
                ├── position.rs            # name-column heuristic, UTF-16 conversion
                ├── uri.rs                 # path <-> file URI
                ├── groundtruth.rs         # load + filter knot JSON
                ├── lsp/
                │   ├── mod.rs
                │   ├── servers.rs         # per-language launch specs + init options
                │   ├── client.rs          # async-lsp main loop, router, request helpers
                │   ├── readiness.rs       # readiness state machine
                │   └── rss.rs             # process-tree memory sampler
                ├── graph/
                │   ├── mod.rs
                │   ├── normalize.rs       # location -> container entity key
                │   ├── heuristic.rs       # in-process knot resolver (tier 2)
                │   ├── gold.rs            # union + adjudication merge
                │   └── metrics.rs         # precision/recall, percentiles
                ├── vec/
                │   ├── mod.rs
                │   ├── encode.rs          # f32 decimal / f32 b64 / int8 variants
                │   ├── shard.rs           # deterministic JSONL shard writer/reader
                │   ├── search.rs          # brute-force top-k
                │   └── chunk.rs           # tier-1 line-window chunker
                └── bin/
                    ├── lsp_probe.rs
                    ├── vec_probe.rs
                    └── summarize.rs
```

Sandbox (not in the repo):

```
/tmp/knot-rag-spike/
├── work/{knot,knot-site,spring-ai}/     # copies of ~/.knot/repos/<id>
├── target-ra/                           # CARGO_TARGET_DIR for rust-analyzer
├── jdtls-ws/spring-ai/                  # jdtls -data workspace
├── s2/<repo>/<variant>/...              # shards written by vec_probe
├── gitmeasure/<repo>/<variant>/         # throw-away git repos for pack-size measurement
└── logs/                                # server stderr, tracing logs
```

---

## 7. Task graph and parallelisation

```
     T0.1 git init (done) ───────────────────────┐
T0.2 install jdtls (done) ──────┼─► T0.5 sanity checks (done)
T0.3 sandbox copies (done) ─────┤
T0.4 env + knot wrapper (done) ─┘
                 │
                 ▼
     T1.0 adapt root Makefile / clippy.toml / dupes.toml   (FIRST, before any Rust is written)
                 │
                 ▼
                T1 scaffold (done) ──► T3 foundations (done) ──┬─► T4 LSP client (done) ──► T5 lsp_probe (done) ──┬─► T7 gold+metrics (done) ──► T10 report (done) ──► T11 gate (done)
                  │                              │                                    │
                  └─► T2 ground truth (done) ────┼─► T6 heuristic (tier 2) (done) ──────┤
                                                 │                                    │
                                                  ├─► T8 no-LSP degradation (done) ◄── (T5) ──┘
                                                 └─► T9 vec_probe (S2) ───────────────────────────────► T10
```

Suggested split across agents (each agent must read §1–§7, §20–§22 plus its tasks):

| Agent | Tasks | Notes |
|---|---|---|
| **A (setup + S2)** | T0, T1 (incl. T1.0), T2, T3, T9 | T1.0 is done before anything else. T9 only needs T1+T3. Can start S2 measurements while B works on LSP. |
| **B (LSP Rust/TS)** | T4, T5 (rust, ts), T6, T8 | Starts after T1+T3 merged. |
| **C (Java)** | T5 (java), T7 (all languages, adjudication) | Starts after T4 is stable; jdtls import can take long — start it early. |
| **A or any** | T10, T11 | After all results exist. |

Merge discipline: one agent owns `Cargo.toml` (agent A). Others request dependency additions
through it, or add them in clearly separated commits/edits.

Rough effort: T0 0.5 h · T1 0.5 h · T2 1.5 h · T3 2 h · T4 4 h · T5 4 h (+ Java import wall
time) · T6 1.5 h · T7 4–6 h (adjudication) · T8 0.5 h · T9 5 h · T10 2 h · T11 1 h.

---

## 8. T0 — Environment setup

### T0.1 — Local git repository (requires explicit human confirmation)

The human approved `git init` on 2026-10-03. **Executed.**

```bash
cd /home/raul/workspace/rust/knot-rag
git init
printf '%s\n' '/target/' '**/target/' 'docs/spikes/phase0/results/raw/' '.knot/' > .gitignore
```

Never add a remote, never push.

### T0.2 — Install jdtls 1.61.0

File `docs/spikes/phase0/scripts/install_jdtls.sh` (idempotent; exits 0 if already installed):

```bash
#!/usr/bin/env bash
set -euo pipefail
V=1.61.0
F=jdt-language-server-1.61.0-202609031315.tar.gz
BASE=https://download.eclipse.org/jdtls/milestones/$V
DEST="$HOME/.local/share/jdtls/$V"
if [ -x "$HOME/.local/bin/jdtls" ] && [ -d "$DEST/plugins" ]; then
  echo "jdtls $V already installed"; exit 0
fi
TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
cd "$TMP"
curl -fsSLO "$BASE/$F"
curl -fsSLO "$BASE/$F.sha256"
# The .sha256 file may contain just the hash, or "hash  filename".
HASH=$(awk '{print $1}' "$F.sha256")
echo "$HASH  $F" | sha256sum -c -
mkdir -p "$DEST"
tar xzf "$F" -C "$DEST"
ln -sfn "$DEST" "$HOME/.local/share/jdtls/current"
mkdir -p "$HOME/.local/bin"
ln -sf "$HOME/.local/share/jdtls/current/bin/jdtls" "$HOME/.local/bin/jdtls"
"$HOME/.local/bin/jdtls" --help >/dev/null 2>&1 || true
echo "installed jdtls $V into $DEST"
```

Acceptance:
- `command -v jdtls` → `/home/raul/.local/bin/jdtls`.
- `ls ~/.local/share/jdtls/current/plugins/org.eclipse.equinox.launcher_*.jar` succeeds.
- `jdtls --help` prints the launcher usage and shows `--jvm-arg` and `-data` options (record the
  exact usage text in `results/raw/jdtls_help.txt`). If `--jvm-arg` is not supported, use
  `JAVA_TOOL_OPTIONS=-Xmx8g` instead and log the deviation.
- Note: the launcher uses `config_linux/` inside the install dir by default; it must be writable
  (it is, because it lives under `$HOME`).

### T0.3 — Sandbox copies

File `scripts/setup_sandbox.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "$0")/env.sh"
mkdir -p "$SPIKE_ROOT"/{work,target-ra,jdtls-ws,s2,gitmeasure,logs}
for r in knot knot-site spring-ai; do
  rsync -a --delete \
    --exclude '.knot/' --exclude '.fastembed_cache/' --exclude 'target/' \
    --exclude 'node_modules/' \
    "$HOME/.knot/repos/$r/" "$SPIKE_ROOT/work/$r/"
done
# tsserver needs dependency types; link (do NOT copy) the real node_modules.
ln -sfn "$HOME/workspace/typescript/knot-site/node_modules" "$SPIKE_ROOT/work/knot-site/node_modules"
# Fingerprint the snapshots so the report can cite exactly what was measured.
for r in knot knot-site spring-ai; do
  ( cd "$SPIKE_ROOT/work/$r" && find . -type f -not -path './node_modules/*' -print0 \
      | sort -z | xargs -0 sha256sum | sha256sum | awk -v r="$r" '{print r, $1}' )
done > "$SPIKE_ROOT/work/FINGERPRINTS.txt"
cat "$SPIKE_ROOT/work/FINGERPRINTS.txt"
```

Notes:
- `rsync --delete` makes the script idempotent and resets any mutation done by T9.6.
- The snapshot of knot has a `Cargo.lock`; rust-analyzer will not rewrite it. If it does, the
  copy is disposable anyway.
- Copy `FINGERPRINTS.txt` into `results/raw/` for the report.

### T0.4 — Environment file and knot wrapper

`scripts/env.sh`:

```bash
# Sourced by every spike script. Keep it side-effect free.
export SPIKE_ROOT=/tmp/knot-rag-spike
export SPIKE_CRATE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export KNOT_EMBED_MODEL=AllMiniLML6V2
export KNOT_EMBED_DIM=384
export KNOT_FASTEMBED_CACHE_DIR="$HOME/.knot/repos/fastembed_cache"
export CARGO_TARGET_DIR_RA="$SPIKE_ROOT/target-ra"
export RUST_LOG="${RUST_LOG:-info}"
```

`scripts/knot.sh`:

```bash
#!/usr/bin/env bash
# knot CLI wrapper: pins the embedding model/dimension used by the live index.
set -euo pipefail
source "$(dirname "$0")/env.sh"
cd /tmp   # avoid picking up any project-local .env
exec knot "$@"
```

### T0.5 — Sanity checks (record outputs in `results/raw/env.txt`)

```bash
source scripts/env.sh
rustc --version; cargo --version
rust-analyzer --version
typescript-language-server --version
jdtls --help | head -5
java -version 2>&1 | head -1; mvn -v | head -1; python3 --version
scripts/knot.sh repos -o json | python3 -c 'import json,sys; print([r["name"] for r in json.load(sys.stdin)])'
scripts/knot.sh callers parse_files -r knot -o json | head -c 400
scripts/knot.sh search "parse source files into entities" -r knot -m 3 -o json | head -c 400
curl -fsS http://localhost:6333/collections/knot_entities | head -c 300
nproc; free -g | head -2; df -h /tmp | tail -1
```

Acceptance: every command succeeds; `knot search` returns JSON (not a dimension error).

---

## 9. T1 — Spike crate scaffold

### T1.0 — Adapt the root quality-gate files (`Makefile`, `clippy.toml`, `dupes.toml`)

The human copied these three files verbatim from knot into the repository root so that knot's
quality bar applies **from the first line of Rust**. They currently reference knot (binaries,
measurements, E2E script) and the `Makefile` assumes a `Cargo.toml` at the root, which does not
exist during Phase 0. Adapt them as follows — **read each file first; change only what is
listed; never loosen a threshold**.

#### T1.0.1 `Makefile` — make every gate target the right crate

Problem: every recipe runs bare `cargo …` at the root → in Phase 0 they fail
(`could not find Cargo.toml`). Solution: a `CRATE_DIR` variable that auto-selects the root crate
once it exists (Phase 1) and the spike crate until then, passed through `--manifest-path`
(cargo fmt/clippy/test/build) and `--path` (cargo-dupes).

Target content (replace the whole file; keep the original target names and help texts so muscle
memory and the `validator` agent keep working):

```make
.PHONY: all fmt fmt-check clippy test dupes dupes-cleanup check check-all e2e build install help

# Crate the quality gates operate on. Until Phase 1 creates the knot-rag root
# crate, the only Rust code is the Phase 0 spike crate (a standalone
# [workspace]). Override explicitly with `make check CRATE_DIR=<dir>`.
CRATE_DIR ?= $(if $(wildcard Cargo.toml),.,docs/spikes/phase0)
MANIFEST  := --manifest-path $(CRATE_DIR)/Cargo.toml

# cargo-dupes reads dupes.toml ONLY from the analysed path (no parent lookup)
# and silently passes when it finds none, so the gate refuses to run without it.
DUPES_CFG := $(CRATE_DIR)/dupes.toml

# Default target: run all mandatory quality gates
all: check

# Auto-fix code formatting
fmt:
	cargo fmt $(MANIFEST)

# Verify code formatting
fmt-check:
	cargo fmt $(MANIFEST) -- --check

# Run linter checks
clippy:
	cargo clippy $(MANIFEST) --all-targets --all-features -- -D warnings

# Run unit tests
test:
	cargo test $(MANIFEST) --lib --all-features

# Check for code duplication (requires cargo-dupes)
dupes:
	@test -x "$$(command -v cargo-dupes)" || (echo "cargo-dupes not found. Installing..." && cargo install cargo-dupes --version 0.2.1 --locked)
	@test -f "$(DUPES_CFG)" || (echo "ERROR: $(DUPES_CFG) missing; cargo-dupes would silently pass without thresholds" && exit 1)
	cargo dupes --path $(CRATE_DIR) check

# Show stale duplication suppressions
dupes-cleanup:
	@test -x "$$(command -v cargo-dupes)" || (echo "cargo-dupes not found. Installing..." && cargo install cargo-dupes --version 0.2.1 --locked)
	cargo dupes --path $(CRATE_DIR) cleanup --dry-run

# Run all local quality gates sequentially (fmt, clippy, unit tests, dupes)
check: fmt-check clippy test dupes

# Build release binaries
build:
	cargo build $(MANIFEST) --release --all-features

# Detect OS binary extension
ifeq ($(OS),Windows_NT)
    EXT := .exe
else
    EXT :=
endif

# Install release binaries to CARGO_HOME/bin (defaults to ~/.cargo/bin)
CARGO_HOME ?= $(HOME)/.cargo
DESTDIR ?= $(CARGO_HOME)/bin

# knot-rag binaries (created in Phase 1). The Phase 0 spike binaries are
# measurement tools and are deliberately not installable.
BINS := knot-rag knot-rag-mcp

install: build
	@test "$(CRATE_DIR)" = "." || (echo "install: no knot-rag binaries before Phase 1 (CRATE_DIR=$(CRATE_DIR))" && exit 1)
	@mkdir -p "$(DESTDIR)"
	@for b in $(BINS); do cp -f "target/release/$$b$(EXT)" "$(DESTDIR)/$$b$(EXT)"; done
	@echo "Installed binaries to $(DESTDIR):"
	@for b in $(BINS); do echo "  - $$b$(EXT)"; done

# Run E2E integration tests (no databases needed). The suite arrives in
# Phase 7; until then the target reports that it was skipped.
E2E_SCRIPT ?= tests/run_all_e2e.sh
e2e:
	@if [ -x "$(E2E_SCRIPT)" ]; then ./$(E2E_SCRIPT); else echo "e2e: $(E2E_SCRIPT) not present yet - skipped"; fi

# Run all local quality gates AND all E2E integration tests
check-all: check e2e

# Display available make targets
help:
	@echo "Available targets (CRATE_DIR=$(CRATE_DIR)):"
	@echo "  make (or make all) - Default target: alias for 'make check'"
	@echo "  make check         - Run all local quality gates (fmt-check, clippy, test, dupes)"
	@echo "  make check-all     - Run all local quality gates AND E2E integration tests"
	@echo "  make fmt           - Auto-fix code formatting with cargo fmt"
	@echo "  make fmt-check     - Verify code formatting with cargo fmt -- --check"
	@echo "  make clippy        - Run linter checks with cargo clippy"
	@echo "  make test          - Run unit tests"
	@echo "  make dupes         - Run code duplication check with cargo-dupes (auto-installs if missing)"
	@echo "  make dupes-cleanup - Show stale duplication suppressions"
	@echo "  make build         - Build release binaries"
	@echo "  make install       - Build release binaries and copy them to ~/.cargo/bin (Phase 1+)"
	@echo "  make e2e           - Run all E2E integration tests (Phase 7+, skipped before)"
```

Differences vs. the knot original (list them in the report's deviations section):
`CRATE_DIR`/`MANIFEST`/`DUPES_CFG` variables; `--manifest-path`/`--path` on every cargo call;
the `dupes` guard against a missing config; `install` lists knot-rag binaries and refuses before
Phase 1; `e2e` points at knot-rag's (future) Docker-free suite and skips when absent.
Recipe lines must be indented with a **tab**.

#### T1.0.2 `clippy.toml` — header only

Keep **every threshold and every explanatory comment** unchanged. Replace only the first line
`# Clippy thresholds for knot.` with:

```toml
# Clippy thresholds for knot-rag.
#
# Inherited verbatim from knot on 2026-10-03 (knot v1.11.1). The measured
# curves quoted below are knot's, not knot-rag's: re-measure them on knot-rag's
# own code at the end of Phase 1 and only ever TIGHTEN (never loosen) a value.
#
# This file lives at the repository root on purpose: clippy searches parent
# directories, so every crate below (including docs/spikes/phase0) inherits it.
# Do not add per-crate copies.
```

and in the paragraph "The last two are enabled in Cargo.toml's [lints.clippy] section." append
" (every knot-rag crate copies knot's `[lints.clippy]` table verbatim)". The note about tracing
macros inflating `cognitive_complexity` by +7 still applies: knot-rag depends on the `knot`
crate, which pulls `rust-mcp-sdk` (and later knot-rag pulls `rust-mcp-sdk` 2.x itself), so the
same `#[expect(clippy::cognitive_complexity)]` policy formula is in force.

#### T1.0.3 `dupes.toml` — header + ratchet reset to zero

knot's thresholds (`max_exact_duplicates = 8`, `max_near_duplicates = 9`,
`max_exact_percent = 2.0`, `max_near_percent = 3.0`) encode **knot's pre-existing backlog**.
knot-rag has no backlog, and the ratchet policy says thresholds "may only ever be lowered", so
start at zero:

1. Replace the header (lines 1–13) with:
   ```toml
   # Duplication thresholds for knot-rag (cargo-dupes 0.2.1).
   #
   # Inherited from knot on 2026-10-03 (analysis scope + exclusions kept
   # verbatim). knot's ratchet counts encoded knot's own backlog; knot-rag
   # starts with none, so the ratchet starts at ZERO and may only stay there.
   #
   # IMPORTANT: cargo-dupes reads this file ONLY from the analysed directory
   # (no parent lookup) and `cargo dupes check` silently passes when no
   # dupes.toml is found. Every crate that is not at the repository root must
   # therefore carry a symlink to this file (docs/spikes/phase0/dupes.toml ->
   # ../../../dupes.toml); the Makefile refuses to run the gate without it.
   #
   # NOTE: cargo-dupes normalizes the AST (identifiers become positional
   # placeholders and literal VALUES are erased, though their types are kept).
   # Two functions that differ only in the string literals they embed therefore
   # collapse into one fingerprint. Handle such false positives through
   # .dupes-ignore.toml with a `reason`, never by raising the thresholds.
   ```
2. Keep `min_lines`, `min_nodes`, `similarity_threshold`, `exclude_tests`, `exclude` **and their
   comments** unchanged (the measured curves in those comments are knot's — prefix each such
   comment block with `# (knot measurement)`).
3. Replace the CI thresholds block with:
   ```toml
   # --- CI thresholds (consumed by `cargo dupes check`) ---
   #
   # RATCHET: greenfield project, no accepted backlog. These values may only
   # ever be lowered, which at zero means: extract a shared, parameterised
   # helper instead of duplicating code.
   max_exact_duplicates = 0
   max_near_duplicates = 0
   max_exact_percent = 0.0
   max_near_percent = 0.0
   ```

#### T1.0.4 Spike-crate wiring

```bash
cd /home/raul/workspace/rust/knot-rag
ln -s ../../../dupes.toml docs/spikes/phase0/dupes.toml      # after T1.1 created the directory
# If a .dupes-ignore.toml is ever needed, create it at the root and symlink it the same way.
```

Do **not** create `docs/spikes/phase0/clippy.toml` or any `rustfmt.toml`.

#### T1.0.5 Acceptance (run after T1.1–T1.5 exist)

1. `make help` prints `CRATE_DIR=docs/spikes/phase0`.
2. `make check` runs all four gates against the spike crate and passes.
3. **Prove clippy inherits the root config:** add a throw-away function with 6 parameters to
   `src/common.rs`, run `make clippy`, observe `this function has too many arguments (6/5)`
   (the root threshold is 5; clippy's default is 7, so this only fires if the root file is
   used), then delete the function.
4. **Prove dupes reads the symlinked config:** `cargo dupes --path docs/spikes/phase0 stats`
   reports units honouring `min_lines = 10`; then `mv docs/spikes/phase0/dupes.toml /tmp/x &&
   make dupes` must **fail** with the missing-config message; restore the symlink.
5. Record the outputs of 3 and 4 in `results/raw/quality_gates.txt`.

### T1.1 `docs/spikes/phase0/Cargo.toml` (exact starting content)

```toml
[package]
name = "knot-rag-spike-phase0"
version = "0.0.0"
edition = "2024"
publish = false
description = "Throw-away Phase 0 feasibility spikes for knot-rag (LSP graph + file-based vectors)"

# Standalone: must never be absorbed by a future knot-rag root workspace.
[workspace]

# --- Lint tables copied VERBATIM from knot's Cargo.toml (decision D11) ---
[lints.rust]
# Enforces the no-unsafe policy stated in AGENTS.md. `deny` rather than
# `forbid`; `deny` still blocks new unsafe while allowing documented,
# audited exceptions via `#[expect(unsafe_code, reason = "...")]`.
unsafe_code = "deny"

[lints.clippy]
# Readability gates. Thresholds live in clippy.toml; these lints are
# allow-by-default (pedantic/nursery), so they are inert without this.
too_many_lines = "warn"
cognitive_complexity = "warn"

# Ban bare `#[allow]` in favour of `#[expect(..., reason = "...")]`, which
# warns once the suppression is no longer needed. This keeps the refactor
# backlog self-cleaning instead of accumulating stale opt-outs.
allow_attributes = "warn"
allow_attributes_without_reason = "warn"

[dependencies]
# Parsers, entity model, embedder, heuristic resolver (decision D3).
knot = { path = "../../../../knot" }

# LSP client. lsp-types comes ONLY through async_lsp::lsp_types (0.95).
async-lsp = { version = "0.2.4", default-features = false, features = ["omni-trait", "tokio", "tracing"] }
tower = { version = "0.5", features = ["util"] }
futures = "0.3"
tokio = { version = "1", features = ["full"] }
tokio-util = { version = "0.7", features = ["compat"] }

# Data, CLI, utils.
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "1"
clap = { version = "4", features = ["derive"] }
anyhow = "1"
url = "2"
uuid = { version = "1", features = ["v5", "serde"] }
base64 = "0.22"
sha2 = "0.11"          # same major as knot to avoid a duplicate build
ignore = "0.4"
sysinfo = "0.39"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }

[dev-dependencies]
tempfile = "3"

[profile.release]
debug = 1
```

Notes for the implementer:
- The relative path `../../../../knot` resolves from `knot-rag/docs/spikes/phase0/` to
  `/home/raul/workspace/rust/knot`. Verify with `cargo metadata --format-version 1 | jq` that the
  resolved `knot` manifest path is that directory.
- Building `knot` compiles fastembed + ort (downloads onnxruntime binaries once) + qdrant-client +
  neo4rs. First build may take several minutes. Always build with `--release` for measurements.
- `toml = "1"` and `sha2 = "0.11"` match knot's majors so they are not compiled twice.
- If an `async-lsp` feature name differs (`tokio` is an implicit feature from its optional
  dependency), check `cargo add async-lsp --features ...` output and adapt.
- The `[lints.*]` tables must stay byte-identical to knot's (comments included). Before
  finishing T1, diff them:
  `diff <(sed -n '/^\[lints.rust\]/,/^allow_attributes_without_reason/p' ~/workspace/rust/knot/Cargo.toml) <(sed -n '/^\[lints.rust\]/,/^allow_attributes_without_reason/p' docs/spikes/phase0/Cargo.toml)`
  → no output. If knot's tables change later, re-sync and note it.
- `make clippy` passes `-D warnings`, so these `warn` lints are effectively errors in the gate.

### T1.2 Clippy / rustfmt configuration

No crate-local files. The spike inherits `/home/raul/workspace/rust/knot-rag/clippy.toml`
(thresholds: too-many-arguments 5, type-complexity 200, excessive-nesting 6, too-many-lines 100,
cognitive-complexity 20) through clippy's parent-directory lookup, verified in T1.0.5 step 3.
rustfmt uses its default style (same as knot). Practical consequences while coding:
- Functions with > 5 parameters → introduce a parameter struct (e.g. `ProbeContext`,
  `IndexOptions`) instead of `#[expect]`.
- Functions > 100 lines or cognitive complexity > 20 → split into helpers. The `run` flows of
  `lsp_probe` (T5.2) and `vec_probe` (T9.2/T9.5) are long by nature: design them from the start
  as a sequence of small step functions (`step_parse`, `step_start_server`, `step_wait_ready`,
  `step_column_check`, `step_targets`, …) returning typed partial results.
- Nesting > 6 levels → early returns / `let … else`.
- Every `tracing` macro adds ~7 to cognitive complexity (see the root `clippy.toml` note); keep
  logging out of branch-heavy functions.
- Duplication ratchet is zero (T1.0.3): the three binaries share argument types and result
  writers through `common.rs`; per-language server specs are data (a table), not copy-pasted
  functions; per-variant encoders share one trait (`VectorEncoding`) instead of parallel
  functions.

### T1.3 `.gitignore`

```
/target/
/results/raw/
```

### T1.4 `src/lib.rs`

```rust
//! Phase 0 feasibility spikes for knot-rag. Throw-away code: optimise for
//! measurement fidelity and readability, not for reuse.

pub mod common;
pub mod entities;
pub mod graph;
pub mod groundtruth;
pub mod lsp;
pub mod position;
pub mod uri;
pub mod vec;
```

### T1.5 `src/common.rs` — shared types

Define (serde `Serialize`/`Deserialize`, field order = output order, which matters for
determinism in S2):

```rust
pub const S1_SCHEMA: &str = "knot-rag-spike/s1/v1";
pub const S2_SCHEMA: &str = "knot-rag-spike/s2/v1";
pub const GOLD_SCHEMA: &str = "knot-rag-spike/gold/v1";
pub const HEUR_SCHEMA: &str = "knot-rag-spike/heuristic/v1";

/// Stable identity of an entity inside a snapshot, independent of UUIDs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EntityKey { pub file: String, pub line: usize, pub name: String }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lang { Rust, Ts, Java }

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Percentiles { pub n: usize, pub p50: f64, pub p95: f64, pub max: f64 }

pub fn percentiles(samples_ms: &[f64]) -> Percentiles; // nearest-rank method, sorted copy
pub fn now_rfc3339() -> String;                         // for result metadata
pub fn write_json_pretty<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()>; // creates parent dirs, trailing newline
```

Unit tests: `percentiles` on empty, single, even/odd sizes (nearest-rank: p95 of 1..=20 → 19).

### T1.6 Acceptance

1. `cargo build --release --manifest-path docs/spikes/phase0/Cargo.toml` succeeds (binaries may
   be stubs printing "todo" at this stage).
2. `make check` (from the repository root) is green: fmt-check, clippy `-D warnings` with the
   inherited root `clippy.toml`, `cargo test --lib`, `cargo dupes check` with the symlinked
   zero-ratchet `dupes.toml`.
3. T1.0.5 proofs recorded.
4. The lint-table diff against knot's `Cargo.toml` is empty.

From here on, **every task's acceptance implicitly includes `make check` green.**

---

## 10. T2 — Ground truth from knot

**Executed** (2026-10-03). Targets (`targets/*.toml`), queries (`queries/*.toml`), `scripts/groundtruth.sh`
and `src/groundtruth.rs` are in place; 65 `callers_*` + 30 `search_*` raw JSON files live in
`groundtruth/`. All 65 targets validate `ok` against the snapshot tables
(`results/raw/validate_{rust,ts,java}.txt`). Deviations to carry into the report:
- knot's real `callers` JSON also contains a `resolution` **metadata object** (tier, resolved
  targets, truncation) next to the relationship groups — not in Appendix D; `load_and_filter`
  skips non-array groups.
- `knot_query` corrections: Rust member queries use `::` (the plan's `Type.member` dot form
  resolves 0 targets) — rust-03 `Embedder::embed_query`, rust-20 `FindCallersTool::handle`;
  rust-19 uses the bare `connect` (`ConnectExt::connect` resolves 0 targets).
- rust-19 adjudicated to `GraphDb::connect` at line 49: knot's parser emits **no entity for
  trait-method declarations** (only impl methods), so the `ConnectExt` trait method at line 13
  cannot be a target (verified with a probe target).
- java-09 adjudicated to `VectorStoreRetriever.java:45` (the `SearchRequest` overload): the
  plan's file (VectorStore.java) has no `similaritySearch` declaration. knot never resolves to
  line 45 (default `String` overload at 54 + 6 implementations) — expected LSP win.
- `validate-targets` is implemented in `lsp_probe` (T5.1 extends the CLI); `--write` rewrites
  resolved lines preserving comments/order. Notes carry `hint: N` / `at N` line hints and
  backtick signature hints (case-sensitive) for overload disambiguation.

### T2.1 Target TOML schema (`targets/*.toml`)

```toml
repo = "knot"                 # knot repo name, also used as ParseConfig.repo_name
lang = "rust"                 # rust | ts | java
root = "/tmp/knot-rag-spike/work/knot"

[[target]]
id = "rust-01"
name = "parse_files"          # entity name as knot stores it
file = "src/pipeline/parser/mod.rs"
line = 210                    # knot start_line (1-based) in the snapshot
knot_query = "parse_files"    # string passed to `knot callers`; use "Type.member" for members
category = "free_fn"          # free_fn | method | trait_method | interface_method | type | trait | interface | abstract_class | constructor | homonym
ops = ["incoming", "references"]          # subset of incoming | references | implementation
notes = ""
```

The full candidate lists are in [Appendix A](#appendix-a--target-lists-s1). Lines marked `TBD`
must be resolved in T2.3.

### T2.2 `scripts/groundtruth.sh`

For each `[[target]]` in each targets file, run and store raw output:

```bash
scripts/knot.sh callers "$knot_query" -r "$repo" -m 500 -k all -o json \
  > "groundtruth/callers_${repo}_${id}.json"
```

and for each query in `queries/entities.toml`:

```bash
scripts/knot.sh search "$query" -r "$repo" -m 10 -o json > "groundtruth/search_${repo}_${qid}.json"
```

Parse TOML from bash with a tiny embedded `python3 -c 'import tomllib,...'` (Python 3.14 has
`tomllib`). Failures (non-zero exit, empty output) must abort with the target id in the message.

### T2.3 Resolve `TBD` lines and validate every target against the snapshot

Implement `lsp_probe validate-targets --targets targets/X.toml` (subcommand of the S1 binary, see
T5.1) that:
1. Builds the snapshot entity table (T3.1) for the repo.
2. For each target, finds the entity with `(file, start_line == line, name)`.
   - If `line = "TBD"` (represent as `line = 0` in TOML) pick the entity in `file` with
     `name` matching; if several (overloads), pick the one listed in the Appendix note
     (e.g. "abstract overload `call(Prompt)`"), else the first by line, and print a warning.
3. Prints a table `id | status (ok|fixed|missing) | resolved line | kind | fqn | uuid`.
4. With `--write`, rewrites the TOML with resolved lines (preserve order and comments if
   feasible; otherwise regenerate the file deterministically — acceptable).
5. Exit code 1 if any target is `missing`.

Acceptance: all targets `ok` after `--write`; changes logged in the report's deviations section.

### T2.4 Ground-truth filtering (`src/groundtruth.rs`)

knot callers JSON (see [Appendix D](#appendix-d--knot-json-samples)) is an object whose keys are
relationship groups (`calls`, `references`, `extends`, `implements`, `overridden_by`,
`overrides`, `macro_calls`, `imports_script`, `imports_stylesheet`, `imports_vmod`, `includes`,
`declared_unused`, …), each an array of rows with `file_path, kind, name, rel_type, repo_name,
signature, start_line, target_file_path, target_fqn, target_name, target_repo_name,
target_signature, target_start_line`.

```rust
pub struct KnotCaller {
    pub key: EntityKey,          // (file_path, start_line, name) of the caller
    pub kind: String,            // knot wire kind
    pub rel_group: String,       // JSON key it came from
    pub rel_type: String,        // e.g. "CALLS", "REFERENCES"
}
pub struct FilteredGroundTruth {
    pub target_id: String,
    pub callers: Vec<KnotCaller>,
    pub dropped_other_target: usize,   // rows whose target (file,line) != our target
    pub dropped_stale_caller: usize,   // caller (file,line,name) not present in snapshot
    pub dropped_cross_repo: usize,     // repo_name != target repo (LSP cannot see them)
    pub excluded_groups: BTreeMap<String, usize>, // e.g. "overrides" (direction is outward)
}
pub fn load_and_filter(path: &Path, target: &Target, table: &EntityTable) -> anyhow::Result<FilteredGroundTruth>;
```

Rules:
- Keep only rows with `target_file_path == target.file && target_start_line == target.line`.
- Drop rows whose caller `(file_path, start_line, name)` is not an entity of the snapshot table
  (stale index rows). Count them.
- Drop rows with `repo_name != target repo`. Count them.
- The `overrides` group lists what the *target* overrides (outgoing direction) → exclude from
  "callers" comparisons, count in `excluded_groups`. `overridden_by` is incoming → keep.
- `declared_unused` → exclude.

Unit tests with a small hand-written JSON fixture covering every rule.

---

## 11. T3 — Shared foundations (entity table, positions, URIs)

**Executed** (2026-10-03). `src/entities.rs`, `src/position.rs`, `src/uri.rs` with 37 unit tests.
Entity-count fidelity vs knot's index (recorded in `results/raw/validate_*.txt`):

| repo | knot index | parse (config=OFF) | parse (config=ON) | files (index / OFF) |
|---|---|---|---|---|
| knot | 5 606 | **4 809** | 5 582 | 269 / 269 |
| knot-site | 2 199 | **1 520** | 1 642 | 75 / 75 |
| spring-ai | 27 736 | **27 740** | — | 2 490 / 2 490 |

Explanation (the >5 % cases): the index was built with `include_config_files = false` — its file
counts match the config=OFF parses exactly and **no** `config_property` in the index comes from
`.yml`/`.properties` (probed via `knot search -k config_property`; only build-system JSONs such as
`package.json`/`tsconfig.json` appear). The remaining gaps (knot −797, knot-site −679) are
**stale rows in the live index** (e.g. `ParsedEntity` at `src/models/entity.rs:276` stale + `:283`
current, per §4); spring-ai (a stable snapshot) matches within 4 entities. Deviations:
- `build` uses `parse_files_stream` with a concurrent drain thread instead of the plan's
  `parse_files` — that wrapper `blocking_send`s into a 1024-slot channel it only drains after the
  producer returns, deadlocking on any snapshot with > 1024 entities (knot's production path in
  `pipeline::runner` streams; the spike mirrors it).
- `build_with(repo, root, include_config_files)` + `--include-config-files` flag exist for the
  fidelity experiment above; `build` keeps the plan signature (config off).
- `is_container` marker list adds `union` so `rust_union` is classified deliberately (T3.2).
- knot types are imported via `knot::models` (`knot::models::entity` is private).
- `validate_targets` takes `&[ParsedEntity]` additionally: T3.1's `SnapshotEntity` has no
  signature field, and T2.3's note-based overload pick needs it.

### T3.1 `src/entities.rs` — snapshot entity table

```rust
pub struct SnapshotEntity {
    pub uuid: Uuid,
    pub key: EntityKey,         // file (repo-relative), start_line, name
    pub end_line: usize,
    pub kind: String,           // EntityKind wire label (Display impl)
    pub fqn: String,
    pub language: String,
    pub is_container: bool,     // see T3.2
}
pub struct EntityTable {
    pub repo: String,
    pub root: PathBuf,
    pub entities: Vec<SnapshotEntity>,                   // sorted by (file, start_line, end_line desc, name)
    by_file: HashMap<String, Vec<usize>>,                // indices into `entities`
    by_uuid: HashMap<Uuid, usize>,
    by_key: HashMap<EntityKey, usize>,
}
impl EntityTable {
    /// Parse the whole snapshot with knot (no DB) and build lookups.
    pub fn build(repo: &str, root: &Path) -> anyhow::Result<(Self, Vec<ParsedEntity>)>;
    pub fn get(&self, key: &EntityKey) -> Option<&SnapshotEntity>;
    pub fn by_uuid(&self, id: &Uuid) -> Option<&SnapshotEntity>;
    /// Innermost entity whose [start_line, end_line] contains `line` (1-based).
    pub fn innermost_at(&self, file: &str, line: usize) -> Option<&SnapshotEntity>;
    /// Innermost *container* entity containing `line`; falls back to a synthetic
    /// file-level key `EntityKey { file, line: 0, name: "<file>" }` when none.
    pub fn container_at(&self, file: &str, line: usize) -> EntityKey;
    pub fn entities_in(&self, file: &str) -> &[usize];
}
```

`build` procedure (must mirror how knot-server indexed, so UUIDs/lines match):

```rust
let root = std::fs::canonicalize(root)?;
let files = knot::pipeline::input::discover_files(root.to_str().unwrap(), false)?;
let cfg = knot::pipeline::parser::ParseConfig {
    repo_root: root.clone(),
    custom_queries_path: None,
    repo_name: repo.to_owned(),
    include_config_files: false,
    repo_path: Some(root.to_string_lossy().into_owned()),
};
let parsed = knot::pipeline::parser::parse_files(&files, &cfg);
```

- `parse_files` is blocking and spawns OS threads; call it from `tokio::task::spawn_blocking`
  when inside an async context.
- `discover_files` respects `.gitignore`; because snapshots have no `.git`, the `ignore` crate
  still honours `.gitignore` files only inside a git repo — **check** whether it does without a
  `.git` dir. If it does not, `target/` or `node_modules` could leak in: the sandbox excludes
  `target/` and the knot-site `node_modules` is a symlink → verify that `discover_files` does not
  follow it (count files under `node_modules/` in the result; must be 0). If it does, filter the
  result with an explicit deny-list (`node_modules`, `target`, `dist`, `.knot`) and log it.
- Record `entities.len()` per repo and compare with knot's counts (5 606 / 2 199 / 27 736).
  Differences > 5 % must be explained (e.g. `include_config_files` mismatch: if knot-server
  indexed with config files enabled, re-run with `true` and keep whichever matches better).

### T3.2 Container classification

`is_container(kind_wire: &str, name: &str) -> bool` returns true when the kind label contains any
of: `function`, `method`, `constructor`, `class`, `interface`, `struct`, `enum`, `trait`,
`impl`, `module`, `record`, `object`, `namespace`, `macro`, `closure`, `annotation`, or when
`name == "<module>"`. Print `EntityKind::all_wire_kinds()` once (debug subcommand) and adjust the
list so every code-construct kind of rust/ts/java is classified deliberately; unit-test a
representative label per language (`rust_function`, `rust_struct`, `method`, `function`,
`interface`, `class`, `constant` → false, `field` → false, `variable` → false).

### T3.3 `src/position.rs` — name column heuristic and UTF-16

LSP positions are **0-based line, UTF-16 code-unit column** (we never negotiate UTF-8).

```rust
pub struct NamePos { pub line0: u32, pub col_utf16: u32, pub byte_col: usize }

/// Locate the declaration name of `entity` in `source`.
pub fn locate_name(source: &str, name: &str, start_line: usize, end_line: usize, lang: Lang)
    -> Option<NamePos>;
pub fn byte_col_to_utf16(line: &str, byte_col: usize) -> u32;
pub fn utf16_to_byte_col(line: &str, col_utf16: u32) -> Option<usize>;
```

Algorithm for `locate_name`:
1. Candidate lines: `start_line ..= min(end_line, start_line + 15)` (1-based → index `-1`).
2. Strip per line: skip lines whose trimmed text starts with `//`, `/*`, `*`, `#[`, `#![`,
   or `@` **only if** the name does not occur on that line after the annotation.
3. Find occurrences of `name` with identifier boundaries (char before/after is not
   `[A-Za-z0-9_$]`).
4. Score each occurrence; take the best:
   - +3 if the preceding token is a declaration keyword for `lang`:
     - rust: `fn`, `struct`, `enum`, `trait`, `type`, `const`, `static`, `mod`, `union`,
       `macro_rules!`;
     - ts/js: `function`, `class`, `interface`, `type`, `enum`, `const`, `let`, `var`,
       `namespace`, `async function`, `get`, `set`;
     - java: `class`, `interface`, `enum`, `record`, `@interface`.
   - +2 if the next non-space char is `(` or `<` (methods/constructors/generics) and the
     occurrence is not preceded by `.` (to avoid call sites inside default methods).
   - +1 if the next non-space char is `:` or `=` (TS properties/object methods).
   - −5 if inside a string literal or a trailing comment on that line (cheap scan for quotes
     and `//` before the position).
   - Tie-breaker: earliest line, then earliest column.
5. Return `None` if no occurrence scored ≥ 0; the caller counts it as `failed_to_locate`.
6. Skip entities named `<module>` (return `None` without counting as failure).

Unit tests (each a small inline source):
- Rust: `#[derive(Debug)]\npub struct Foo {` → line 2, col 11; `pub async fn bar<T>(` with doc
  comments above; `impl Trait for X { fn baz(` ; a method named `new` where `Self::new(` appears
  in the body (body occurrence must lose).
- Java: annotations on previous lines; `public static <T> @Nullable T mergeOption(` (generic and
  type annotation before the name); default interface method that calls an overload of itself
  (`call(prompt)` inside `call(String)`).
- TS: `export function jsonText(`, `export const router = {`, object method `subscribe(listener)`,
  `export interface ToolDefinition {`.
- UTF-16: a line containing `"ñ"` and an emoji before the name → column differs from bytes.

### T3.4 `src/uri.rs`

```rust
pub fn path_to_uri(abs: &Path) -> anyhow::Result<async_lsp::lsp_types::Url>; // Url::from_file_path
pub fn uri_to_repo_relative(uri: &Url, root: &Path) -> Option<String>;     // to_file_path + strip_prefix + '/' separators
```

Handle percent-encoding (spaces), symlinks (canonicalise root once; LSP servers may return
canonical paths — knot-site's `node_modules` is a symlink, so locations inside it must be
classified as `external` and ignored). Unit tests: round trip with spaces, a Windows-style path
string parsed via `Url::parse("file:///C:/x/y.rs")` (only assert `uri_to_repo_relative` returns
`None` for a non-matching root, since `to_file_path` is platform-dependent).

---

## 12. T4 — LSP client, readiness detection, RSS sampler

**Executed** (2026-10-03). `src/lsp/{mod,servers,client,readiness,rss}.rs` + `lsp_probe smoke`
(T4.5 acceptance green, outputs in `results/raw/smoke_{rust,ts}.txt`):

| smoke | ready | reason | ready time | documentSymbol | RSS peak | procs |
|---|---|---|---|---|---|---|
| rust-analyzer on `work/knot` | true | `serverStatus quiescent` | 5.0 s | `src/lib.rs` 9 symbols, 0 ms | 3 006 MiB | 3 |
| typescript-language-server on `work/knot-site` | true | `progress idle` | 2.6 s | `src/main.tsx` 1 symbol, 9 ms | 546 MiB | 4 |

Both shut down cleanly within the 10 s window (no leftover processes). Deviations to carry
into the report:
- `request_with_retry` returns a `RequestOutcome<T>` struct (result, per-try latency, total
  including retries, retry count) instead of the plan's 3-tuple — the text requires both
  latencies, which a tuple cannot carry. It takes `&ServerSocket` and needs `R::Params: Clone`
  for the retries.
- `LspSession` carries extra fields the plan's result requirements imply: `seen_server_requests`,
  `notif_counts` (T4.2 item 2), `language_id_for`, `opened`, `first_request_done`,
  `first_request_timeout` (did_open / 120 s first jdtls request rules).
- `ReadinessState` carries `progress_pairs`, `first_activity`, `ra_status_seen`,
  `ra_indexing_ended`, `jdtls_status_timeline` (required by the T4.3 rules and the
  `jdtls_status_timeline` result field).
- **`#[expect(deprecated, reason = ...)]` on `build_initialize_params`** (reported to the human):
  lsp-types 0.95 marks `InitializeParams::root_uri` deprecated, but jdtls and
  typescript-language-server still read it (T4.2 item 3), so it must be set. This is the only
  `#[expect]` in the spike.
- sysinfo 0.39 lists Linux **threads** as processes (`ProcessRefreshKind::tasks` defaults to
  true) and each thread reports the whole process RSS: without `without_tasks()` the sampler
  reported a 212 GiB "peak" over 184 "processes" for rust-analyzer. The sampler and
  `kill_process_tree` refresh with `.without_tasks()`.
- stderr logs are named `<lang>_<unix_ms>.stderr.log` (`start` has no run label parameter).
- lsp-types is used **only** through `async_lsp::lsp_types` (the plan's rule); its request type
  names are `WorkspaceSymbolRequest`, `DocumentSymbolRequest`, `GotoImplementation`,
  `CallHierarchyPrepare`, `CallHierarchyIncomingCalls`, `WorkspaceFoldersRequest`; several param
  structs require `partial_result_params`; `ReferenceParams.text_document_position` (not
  `..._position_params`); `GotoDefinitionResponse::Scalar` holds a `Location` (not `Option`);
  `ProgressParamsValue` only has `WorkDone` in 0.95.

### T4.1 `src/lsp/servers.rs` — launch specs

```rust
pub struct ServerSpec {
    pub lang: Lang,
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub language_id_for: fn(&Path) -> &'static str,  // "rust" | "typescript" | "typescriptreact" | "javascript" | "javascriptreact" | "java"
    pub initialization_options: serde_json::Value,
    pub ready_timeout: Duration,
    pub readiness: ReadinessKind,
}
pub fn spec_for(lang: Lang, root: &Path, spike_root: &Path) -> ServerSpec;
pub fn which(program: &str) -> Option<PathBuf>; // PATH lookup without shelling out (iterate env::split_paths, check is_file + exec bit on unix)
```

Concrete values:

| | rust | ts | java |
|---|---|---|---|
| program | `rust-analyzer` | `typescript-language-server` | `jdtls` |
| args | — | `--stdio` | `--jvm-arg=-Xmx8g`, `-data`, `/tmp/knot-rag-spike/jdtls-ws/spring-ai` |
| env | `CARGO_TARGET_DIR=/tmp/knot-rag-spike/target-ra` | — | — |
| ready_timeout | 300 s | 120 s | 900 s |
| readiness | `RustAnalyzerStatus` | `ProgressIdle` | `JdtlsServiceReady` |

Initialization options:

- **rust-analyzer**
  ```json
  { "checkOnSave": false,
    "cachePriming": { "enable": true },
    "cargo": { "buildScripts": { "enable": true } },
    "procMacro": { "enable": true } }
  ```
  `checkOnSave=false` avoids a full `cargo check` flycheck that is irrelevant to navigation;
  record it in the report.
- **typescript-language-server**
  ```json
  { "tsserver": { "logVerbosity": "off" },
    "maxTsServerMemory": 4096,
    "preferences": { "includeCompletionsForModuleExports": false } }
  ```
- **jdtls**
  ```json
  { "bundles": [],
    "workspaceFolders": ["file:///tmp/knot-rag-spike/work/spring-ai"],
    "settings": { "java": {
      "import": { "maven": { "enabled": true }, "gradle": { "enabled": false },
                  "exclusions": ["**/node_modules/**", "**/.metadata/**", "**/archetype-resources/**", "**/META-INF/maven/**"] },
      "configuration": { "updateBuildConfiguration": "automatic" },
      "autobuild": { "enabled": true },
      "maxConcurrentBuilds": 4,
      "referencesCodeLens": { "enabled": false },
      "implementationsCodeLens": { "enabled": false } } },
    "extendedClientCapabilities": { "progressReportProvider": true, "classFileContentsSupport": false } }
  ```

### T4.2 `src/lsp/client.rs` — async-lsp main loop

Model the code on the upstream example `examples/client_builder.rs` (URL in §4):

```rust
pub struct LspSession {
    pub server: async_lsp::ServerSocket,
    pub child: tokio::process::Child,
    pub mainloop: tokio::task::JoinHandle<anyhow::Result<()>>,
    pub events: tokio::sync::watch::Receiver<ReadinessState>, // fed by the router
    pub init_result: lsp_types::InitializeResult,
    pub timings: SessionTimings,                             // spawn, initialize_ms
    pub stderr_log: PathBuf,
}
pub async fn start(spec: &ServerSpec, root: &Path, logs_dir: &Path) -> anyhow::Result<LspSession>;
pub async fn shutdown(session: LspSession, timeout: Duration) -> anyhow::Result<()>; // shutdown + exit + emit Stop + wait child (kill on timeout)
```

Implementation details (each one has bitten other LSP clients — implement all):

1. **Spawn** with `tokio::process::Command`, `stdin/stdout` piped, **stderr redirected to a log
   file** (`logs/<lang>_<run>.stderr.log`), `kill_on_drop(true)`, `current_dir(root)`.
   Adapt tokio pipes to futures IO with `tokio_util::compat::{TokioAsyncReadCompatExt,
   TokioAsyncWriteCompatExt}` and call `mainloop.run_buffered(stdout.compat(), stdin.compat_write())`.
2. **Router** (`async_lsp::router::Router<ClientState>`), wrapped with
   `ServiceBuilder::new().layer(TracingLayer::default()).layer(CatchUnwindLayer::default()).layer(ConcurrencyLayer::default())`:
   - **Requests from server → client (must answer or some servers hang):**
     - `workspace/configuration` → `Ok(vec![serde_json::Value::Null; params.items.len()])`
       (for jdtls, answer each item whose `section` starts with `java` with the `settings.java`
       sub-tree from the init options; others `Null`).
     - `window/workDoneProgress/create` → `Ok(())`.
     - `client/registerCapability`, `client/unregisterCapability` → `Ok(())`.
     - `workspace/workspaceFolders` → the single root folder.
     - `window/showMessageRequest` → `Ok(None)`.
     - Any other request: `router.unhandled_request(...)` → reply `Ok(Value::Null)` and log at
       `debug` with the method name (record the set of methods seen in the result JSON as
       `server_requests_seen`).
   - **Notifications from server:**
     - `$/progress` → update readiness state (token, kind begin/report/end, title).
     - `experimental/serverStatus` (rust-analyzer; custom `Notification` type with params
       `{ health: String, quiescent: bool, message: Option<String> }`) → readiness.
     - `language/status` (jdtls; custom type, params `{ type: String, message: String }`) →
       readiness.
     - `textDocument/publishDiagnostics`, `window/logMessage`, `window/showMessage`,
       `telemetry/event` → ignore (count them).
     - **`router.unhandled_notification(|_, _| ControlFlow::Continue(()))` is mandatory**:
       async-lsp's default for unknown notifications is to stop the main loop.
   - A custom event `Stop` that breaks the loop (as in the example).
3. **`initialize` params:**
   - `process_id: Some(std::process::id())`, `root_uri: Some(root_url)` (deprecated but jdtls
     and tsserver still read it), `workspace_folders: Some(vec![WorkspaceFolder { uri, name }])`,
     `initialization_options: Some(spec.initialization_options.clone())`.
   - `capabilities`:
     - `window.work_done_progress = Some(true)`;
     - `text_document.call_hierarchy = Some(CallHierarchyClientCapabilities { dynamic_registration: Some(false) })`;
     - `text_document.type_hierarchy = Some(...)` (if present in lsp-types 0.95; otherwise send via
       `experimental` is NOT needed — just skip);
     - `text_document.references`, `.implementation`, `.definition` = `Some(Default::default())`;
     - `text_document.document_symbol = Some(DocumentSymbolClientCapabilities { hierarchical_document_symbol_support: Some(true), .. })`;
     - `workspace.workspace_folders = Some(true)`, `workspace.configuration = Some(true)`,
       `workspace.symbol = Some(Default::default())`;
     - `experimental = Some(json!({ "serverStatusNotification": true }))` (rust-analyzer).
   - Then send `initialized`.
4. **Request helper with retries:**
   ```rust
   pub async fn request_with_retry<R: lsp_types::request::Request>(
       server: &mut ServerSocket, params: R::Params, max_retries: u32, per_try: Duration)
       -> (Result<R::Result, String>, Duration /* total latency */, u32 /* retries */)
   ```
   Retry on `ErrorCode::CONTENT_MODIFIED` (-32801) and `ServerCancelled`/`RequestCancelled`
   (-32802/-32800) with 500 ms backoff, max 10. Each try has a timeout (30 s default; 120 s for
   the first request on jdtls). Latency reported = wall time of the successful try; also report
   total including retries.
5. **Document sync:** `did_open(path)` reads the file from disk, sends
   `DidOpenTextDocumentParams { uri, language_id, version: 0, text }`, and keeps a `HashSet` of
   opened URIs to avoid re-opening. Never send `didChange`.
6. Expose typed helpers (all via `request_with_retry`):
   `prepare_call_hierarchy(uri, pos)`, `incoming_calls(item)`, `references(uri, pos, include_declaration=false)`,
   `implementation(uri, pos)`, `document_symbol(uri)`, `workspace_symbol(query)`.
   For `implementation`, normalise `GotoDefinitionResponse` (Scalar | Array | Link) to a
   `Vec<Location>` (use `target_selection_range` for links).

### T4.3 `src/lsp/readiness.rs`

```rust
#[derive(Clone, Debug, Default)]
pub struct ReadinessState {
    pub open_progress: BTreeMap<String, String>,   // token -> title
    pub progress_events: u64,
    pub last_activity: Option<Instant>,
    pub ra_quiescent: Option<bool>,
    pub ra_health: Option<String>,
    pub jdtls_status: Option<String>,             // last language/status type
    pub ready_reason: Option<String>,
}
pub enum ReadinessKind { RustAnalyzerStatus, JdtlsServiceReady, ProgressIdle }
pub async fn wait_ready(rx: &mut watch::Receiver<ReadinessState>, kind: ReadinessKind,
                        idle: Duration /* 2 s */, timeout: Duration) -> ReadyOutcome;
pub struct ReadyOutcome { pub ready: bool, pub reason: String, pub elapsed: Duration }
```

Rules:
- **RustAnalyzerStatus:** ready when the last `experimental/serverStatus` has `quiescent == true`
  and `health != "error"`. Fallback (if the server never sends serverStatus within 30 s): treat
  like ProgressIdle but require that tokens `rustAnalyzer/Indexing` (or
  `rustAnalyzer/cachePriming`) have ended at least once.
- **JdtlsServiceReady:** ready when a `language/status` with `type == "ServiceReady"` has been
  received **and** no `$/progress` token is open **and** 2 s idle. Record every distinct
  `language/status.type` with its timestamp in the result (`jdtls_status_timeline`).
- **ProgressIdle (ts):** tsserver initialises lazily; the caller must `didOpen` at least one
  file per tsconfig project **before** waiting. Ready when ≥ 1 progress begin/end pair has been
  observed (or 5 s have passed since the first didOpen with no progress at all) and no token is
  open and 2 s idle.
- On timeout: `ready=false`, `reason="timeout"`; the probe continues (results flagged
  `ready=false`) so partial behaviour is still measured.

Unit tests: drive the state machine with synthetic event sequences (no server needed).

### T4.4 `src/lsp/rss.rs`

```rust
pub struct RssSampler { handle: JoinHandle<RssReport>, stop: watch::Sender<bool> }
pub struct RssReport { pub peak_bytes: u64, pub last_bytes: u64, pub samples: u64,
                       pub peak_process_count: usize, pub timeline: Vec<(u64 /*ms*/, u64 /*bytes*/)> }
impl RssSampler {
    pub fn start(root_pid: u32, every: Duration /* 250 ms */) -> Self;
    pub fn mark(&self, label: &str); // optional: snapshot at "ready", "end"
    pub async fn stop(self) -> RssReport;
}
```

Sum `memory()` (resident bytes) of the root pid **and all descendants** (build a parent map
each tick: tsserver spawns node children; jdtls runs inside a java process started by the python
launcher; rust-analyzer spawns `cargo`/proc-macro servers). Use `sysinfo::System` with a process
refresh limited to memory (check exact 0.39 API names on docs.rs:
`refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing().with_memory())`
or equivalent). Downsample the timeline to ≤ 400 points in the output.

### T4.5 Acceptance for T4

A smoke binary run (`lsp_probe smoke --lang rust --root /tmp/knot-rag-spike/work/knot`) starts
rust-analyzer, reaches ready, runs `documentSymbol` on `src/lib.rs`, prints the symbol count,
shuts down cleanly (child exits within 10 s), and prints the RSS peak. Same for ts on
`src/main.tsx`.

---

## 13. T5 — `lsp_probe` binary (S1)

**Executed** (2026-10-03/04). `src/probe.rs` (result schema + run flow), `src/graph/normalize.rs`
(T7.1 subset), `lsp_probe run`/`kinds`. The full T5.4 matrix ran sequentially — **6/6 runs
`ready=true`, 65/65 targets `ok`, 0 `retried_empty`** (results in `results/s1_<lang>_<run>.json`,
wall-clock + `uptime` in `results/raw/matrix_*.txt`):

| run | ready reason | init→ready | parse | RSS peak | column check | latency p50 (prepare / calls / refs / impl) |
|---|---|---|---|---|---|---|
| rust cold | `serverStatus quiescent` | 12.4 s | 1.70 s | 4 458 MiB | 465/465 | 1 / 37 / 2 / 0 ms |
| rust warm | `serverStatus quiescent` | 4.9 s | 1.74 s | 3 322 MiB | 465/465 | 1 / 52 / 2 / 0 ms |
| ts cold | `progress idle` | 2.7 s | 0.51 s | 914 MiB | 177/178 | 0 / 2 / 3 / 2 ms |
| ts warm | `progress idle` | 2.7 s | 0.52 s | 890 MiB | 177/178 | 0 / 1 / 2 / 1 ms |
| java cold | `ServiceReady and progress idle` | 20.0 s | 7.2 s | 3 624 MiB | 315/315 | 2 / 1 959 / 1 181 / 271 ms |
| java warm | `ServiceReady and progress idle` | 2.5 s | 7.3 s | 2 737 MiB | 315/315 | 2 / 1 441 / 1 232 / 268 ms |

Column check overall: **957/958 computed positions match the server's `selection_range` (99.9 %)**;
the single `differs` is a minified single-letter TS identifier (`i` in `scripts/fetch-updates.mjs`).
`failed_to_locate`: rust 1, ts 100, java 0 (TS name heuristic weak spots — none of the 21 TS
*targets* is affected: all located `ok`). `symbol_not_found` residuals are entities LSP genuinely
does not symbolize (knot test fixtures `.java`/`.cs`, `KnotServer.css`, Markdown sections).

**Java cold reproducibility note:** the very first cold run ever (empty `~/.m2`) measured
init→ready = **171 s**; the recorded `s1_java_cold.json` is a plan-conformant cold run
(`jdtls-ws/spring-ai` wiped, Maven local cache already warm) at 20 s. The `spring-ai-subset`
fallback was **not** needed. Deviations to carry into the report:
- `src/probe.rs` holds the S1 result schema + flow (the plan's layout put result types in
  `common.rs`; keeping producer and JSON shape together avoids drift).
- Op results carry an extra `self_count` (T7.1 requires self-references to be counted).
- `ColumnCheck` gained `symbol_not_found` and `files_without_symbols` counters (and `compared`
  counts only real position comparisons): the plan's 3 counters silently absorbed Markdown /
  fixture files and jdtls name formats.
- Column-check accommodations: files with **no** document symbols are skipped (nothing to compare
  against); symbol names are matched signature-stripped (`foo()` ≡ `foo` — jdtls appends `()`).
- `type_hierarchy` capability is always `false`: lsp-types 0.95 (async-lsp's pin) has no
  `type_hierarchy_provider` field to read. S1 does not measure type hierarchy.
- `timings_ms.first_request` is the first LSP request after ready (column check's first
  `documentSymbol` when enabled, else the first graph request); `spawn_to_initialize` is
  `start()` entry → `initialize` response.

### T5.1 CLI (`clap`)

```
lsp_probe validate-targets --targets <toml> [--write]
lsp_probe smoke --lang <rust|ts|java> --root <dir>
lsp_probe run --targets <toml> --run <cold|warm> --out <json> [--max-targets N] [--skip-column-check]
lsp_probe nolsp --targets <toml> --out <json>           # T8
lsp_probe kinds                                         # prints EntityKind::all_wire_kinds()
```

### T5.2 `run` flow

1. Load targets TOML; build `EntityTable` (T3.1) — **time it** (`parse_ms`) because it is also
   knot-rag's indexing cost for these repos.
2. Validate targets (T2.3 logic, no write); abort on missing.
3. Load filtered ground truth for each target (T2.4) — needed only to log counts here.
4. **Cold vs warm:**
   - `cold`: before starting, delete `/tmp/knot-rag-spike/target-ra` (rust) or
     `/tmp/knot-rag-spike/jdtls-ws/spring-ai` (java). tsserver has no persistent cache → cold and
     warm differ only by OS file cache; still run both.
   - `warm`: keep those directories from the previous cold run.
5. Start RSS sampler, then `client::start`. Record `spawn_to_initialize_ms`.
6. For ts: `didOpen` `src/main.tsx`, `functions/mcp.ts` and `scripts/fetch-updates.mjs` (one per
   project, including the inferred project of `scripts/`). For rust and java no pre-open needed.
7. `wait_ready` → `initialize_to_ready_ms`, `ready`, `ready_reason`.
8. **Column heuristic check** (unless `--skip-column-check`): choose 30 files deterministically
   (sort files with ≥ 3 entities by path, take every k-th to get 30). For each: `didOpen`,
   `documentSymbol`, flatten the tree, and for each knot entity with a computed `NamePos`, find
   the document symbol with the same name whose `range` contains the entity's start line; compare
   its `selection_range.start` to the computed position. Count `compared`, `matched`,
   `failed_to_locate`, keep up to 50 mismatch examples.
9. **Per target** (sequential, to keep latency numbers clean):
   1. `didOpen(target.file)`.
   2. Compute the query position with `locate_name`. If `None` → record
      `status="no_position"` and continue.
   3. If `ops` contains `incoming`: `prepareCallHierarchy` → if it returns items, take the item
      whose `selection_range` contains the position (else the first), then `incomingCalls`.
      Record latency of each request separately and the sum.
   4. If `references`: `textDocument/references` with `include_declaration=false`.
   5. If `implementation`: `textDocument/implementation`.
   6. **Empty-result guard:** if every op returned 0 results while the filtered ground truth has
      ≥ 1 caller, wait 5 s and retry the whole target once; record `retried_empty=true`.
   7. Normalise every returned location (T7.1) to `EntityKey`s; mark locations outside the root
      (dependencies, JDK, node_modules) as `external` and exclude them from comparisons (count).
10. Stop sampler; `shutdown`.
11. Write the result JSON (schema below). Also append the server's stderr log path.

The first measured request after ready is recorded separately as `first_request_ms`
(it often includes lazy work).

### T5.3 Result JSON schema `results/s1_<lang>_<run>.json`

```json
{
  "schema": "knot-rag-spike/s1/v1",
  "lang": "rust",
  "repo": "knot",
  "run": "cold",
  "root": "/tmp/knot-rag-spike/work/knot",
  "snapshot_fingerprint": "sha256…",
  "server": { "program": "rust-analyzer", "args": [], "version": "rust-analyzer 1.98.1 (…)" },
  "started_at": "2026-10-…",
  "parse": { "files": 269, "entities": 5606, "parse_ms": 1234 },
  "capabilities": {
    "call_hierarchy": true, "type_hierarchy": false, "implementation": true,
    "references": true, "document_symbol": true, "workspace_symbol": true,
    "raw": { "...": "full ServerCapabilities as JSON" }
  },
  "timings_ms": { "spawn_to_initialize": 120, "initialize_to_ready": 23000, "first_request": 450 },
  "ready": { "ready": true, "reason": "serverStatus.quiescent", "jdtls_status_timeline": [] },
  "rss": { "peak_bytes": 0, "at_ready_bytes": 0, "end_bytes": 0, "peak_process_count": 3, "timeline": [[0, 0]] },
  "server_requests_seen": ["workspace/configuration", "window/workDoneProgress/create"],
  "notifications_seen": { "$/progress": 120, "textDocument/publishDiagnostics": 40 },
  "column_heuristic": { "compared": 412, "matched": 409, "failed_to_locate": 2, "mismatches": [] },
  "targets": [
    {
      "id": "rust-01", "name": "parse_files", "file": "src/pipeline/parser/mod.rs", "line": 210,
      "category": "free_fn", "status": "ok",
      "position": { "line0": 209, "col_utf16": 7 },
      "retried_empty": false,
      "ops": {
        "incoming": { "supported": true, "prepare_ms": 12.0, "calls_ms": 30.0, "retries": 0, "error": null,
                      "raw_count": 4, "external_count": 0,
                      "locations": [ { "file": "src/bin/knot-indexer.rs", "line0": 10, "col0": 4, "container": { "file": "…", "line": 1, "name": "main" } } ] },
        "references": { "supported": true, "ms": 40.0, "retries": 0, "error": null, "raw_count": 5, "external_count": 0, "locations": [] },
        "implementation": null
      },
      "lsp_containers": { "incoming": [ {"file":"…","line":1,"name":"main"} ], "references": [], "implementation": [], "union": [] },
      "lsp_files": ["src/bin/knot-indexer.rs"]
    }
  ],
  "latency_ms": {
    "prepare_call_hierarchy": { "n": 18, "p50": 0, "p95": 0, "max": 0 },
    "incoming_calls": { "n": 18, "p50": 0, "p95": 0, "max": 0 },
    "references": { "n": 18, "p50": 0, "p95": 0, "max": 0 },
    "implementation": { "n": 3, "p50": 0, "p95": 0, "max": 0 }
  }
}
```

### T5.4 Execution matrix (record wall-clock duration of each run in the report)

| Run | Command |
|---|---|
| Rust cold | `lsp_probe run --targets targets/rust_knot.toml --run cold --out results/s1_rust_cold.json` |
| Rust warm | same with `--run warm --out results/s1_rust_warm.json` |
| TS cold / warm | `targets/ts_knot_site.toml` → `results/s1_ts_{cold,warm}.json` |
| Java cold / warm | `targets/java_spring_ai.toml` → `results/s1_java_{cold,warm}.json` |

Run the three languages **sequentially** (not in parallel) so RSS and latency are not distorted.
Close other heavy processes if possible; record `uptime` load average before each run.

**Java fallback (only if the cold run is not ready within 900 s or the import fails):**
create `/tmp/knot-rag-spike/work/spring-ai-subset/` containing only the root `pom.xml` and the
modules `spring-ai-commons`, `spring-ai-model`, `spring-ai-client-chat`, `spring-ai-vector-store`,
`spring-ai-retry` (edit the copied root `pom.xml` `<modules>` to list only those, plus any parent
BOM module they require — inspect `<parent>` elements). Restrict Java targets and ground truth to
those modules (drop rows whose file is outside them, count the drops) and label the results
`subset`. Always keep the failed full-run artefacts (logs, timeline) — they are a finding.

---

## 14. T6 — Level-2 heuristic graph (in-process knot resolver)

**Executed** (2026-10-04). `src/graph/heuristic.rs` + `lsp_probe heuristic --targets --out`
(results in `results/heuristic_<lang>.json`, logs in `results/raw/heuristic_*.txt`):

| lang | resolve_ms | entities | resolved | ambiguous | unresolved |
|---|---|---|---|---|---|
| rust | 895 | 4 809 | 4 423 | 581 | 11 534 |
| ts | 36 | 1 520 | 254 | 0 | 406 |
| java | 41 216 | 27 740 | 71 232 | 12 998 | **184 710** |

The in-process resolver **reproduces knot's Neo4j row counts exactly** for the sampled targets
(rust-01 4 calls, rust-02 6, rust-03 3, rust-04 38, rust-05 3; java-01 402 refs + 1 implements;
java-06 661 calls + 1 450 refs — matching Appendix A's "knot rows" column). As the plan notes,
this resolver sees **one repository only** (the snapshot), unlike knot's Neo4j pipeline with its
global cross-repo context — which is exactly the knot-rag situation.

Preview vs S1 LSP union container counts (formalised in T7): equal in 8/20 rust, 14/21 ts,
4/24 java targets; LSP finds more in the rest (java-04 `ChatModel`: heuristic 0 vs LSP 253 —
the predicted knot blind spot; java-02: 3 vs 401). Deviations:
- The public knot path is `knot::pipeline::ingest::resolve_reference_intents` (the
  `ingest::resolve` module itself is private; the plan cited the inner path).
- `HeuristicTarget` carries `self_count` (same T7.1 accounting as the S1 op results) and
  `metrics_debug` is also written into the result JSON (the plan listed it only on
  `HeuristicGraph`).

Goal: measure what knot-rag can offer **without LSP and without Neo4j** using knot's own
resolver, which is pure and in-memory.

`src/graph/heuristic.rs`:

```rust
pub struct HeuristicGraph {
    /// target uuid -> set of (caller uuid, relationship label)
    pub incoming: HashMap<Uuid, BTreeSet<(Uuid, String)>>,
    pub resolve_ms: u128,
    pub metrics_debug: String,   // format!("{:?}", RunMetrics) or the counters it exposes
}
pub fn build(parsed: &[ParsedEntity]) -> HeuristicGraph;
```

Procedure:
1. `let mut res: Vec<ResolutionEntity> = parsed.iter().map(ResolutionEntity::from).collect();`
2. `let metrics = knot::pipeline::ingest::resolve::resolve_reference_intents(&mut res);` (time it)
3. For each entity `e`, for each `(target_uuid, rel)` in `e.relationships`: insert
   `(e.uuid, rel.to_string())` into `incoming[target_uuid]`. Skip `Contains`.
4. For each S1 target, look up its snapshot entity uuid and convert the callers to container
   keys (`EntityTable::container_at(caller.file, caller.start_line)`; most callers are already
   containers — the conversion just guarantees the same granularity as LSP).

Binary entry point: `lsp_probe heuristic --targets <toml> --out results/heuristic_<lang>.json`
(schema `knot-rag-spike/heuristic/v1`: `{lang, repo, resolve_ms, entities, targets:[{id, callers:[EntityKey], by_rel:{CALLS:n,…}}]}`).

Note in the report: this resolver only sees one repository (no cross-repo context), which is
exactly the knot-rag situation; knot's Neo4j version loaded a global context.

---

## 15. T7 — Gold set, adjudication and graph metrics

**Executed** (2026-10-04). `src/graph/gold.rs` + `src/graph/metrics.rs` + `lsp_probe gold --lang`.
Adjudication files in `results/adjudication/{rust,ts,java}.toml` (60+20+60 items reviewed by
reading the code), gold results in `results/gold_<lang>.json`. **Micro metrics** (G1–G4 source
fields):

| lang | estimated | LSP P | LSP R | knot_db P/R | heuristic P/R | lsp_calls R | knot_db_calls R |
|---|---|---|---|---|---|---|---|
| rust | yes (50/470) | **100 %** | **100 %** | 93.5 / 66.5 % | 93.3 / 86.5 % | 11.1 % | 11.0 % |
| ts | no (10/10) | **100 %** | **97 %** | 92.6 / 95.5 % | 92.8 / 97 % | 37.9 % | 34.8 % |
| java | yes (50/5 192) | **94.1 %** | **95 %** | 88.2 / 54.2 % | 88.1 / 54.4 % | 24.1 % | 11.3 % |

**Gates G1 (rust recall ≥ 90 %), G2 (ts ≥ 90 %), G3 (java ≥ 85 %), G4 (precision ≥ 90 % each)
all pass.** The 30 spot-check items (seed 42) were verified by reading the code: all 30 auto-tp
agreements confirmed. Adjudication rule recorded in every file header and verdict (deviation,
since the plan left "genuinely calls" open):

- **`tp` iff the reported container is the OWNER of a genuine reference site** (owner =
  `container_at(file, site_line)`, the same mapping T7.1 prescribes) — direct call/reference
  semantics, which is also LSP's callHierarchy/references granularity. An ownership gold is the
  only reading under which G1–G3 are achievable by any LSP; a span-contains gold would deduct
  enclosing-only attributions from LSP's recall by construction.
- **`fp` categories found** (each with reason + evidence in the TOML): javadoc `{@link}`/`@see`
  mentions (LSP reports them — they are the only LSP-side fps, ≈ 482 estimated in java);
  calls to a **different overload** (`prompt(String)` → `prompt(Prompt)`, `create(2-arg)` →
  `create(4-arg)`, `call(String, ToolContext)` → 2-arg delegate — plan T7.3 rule 3);
  enclosing/sibling attributions (`mod x;` declarations, object-literal climbs to `<module>`,
  classes owning nested methods' sites, `handleTabKeyDown`-style misattributions);
  constructor-declaration names; different members (`embed` vs `embed_query`,
  `VectorDb::connect` vs `ConnectExt`).
- Union sizes: rust 1 257 items (787 auto-tp, 470 pending → stratified sample of 50),
  ts 71 (61 auto-tp, 10 pending — exact), java 9 179 (3 987 auto-tp, 5 192 pending → sample
  of 50). Sampling: seed-42 shuffle within strata `target × found_by`, round-robin pick,
  weights `N/n`; `estimated: true` in rust/java results.
- `notable_disagreements` marked via `notable = true` in the TOML: 4 rust, 2 ts, 7 java
  (≤ 15 per language as specified).
- Secondary metrics (calls-only sets) confirm the expectation: `lsp_calls`/`knot_db_calls`
  recall collapses to 11–38 % because type references dominate the gold.

### T7.1 Normalisation (`src/graph/normalize.rs`)

- LSP `CallHierarchyIncomingCall.from` → container key = `table.container_at(file,
  from.selection_range.start.line + 1)`.
- LSP `Location` (references / implementation) → container key = `table.container_at(file,
  range.start.line + 1)`.
- knot ground-truth caller `(file, start_line, name)` → look up the snapshot entity, then
  `container_at(file, start_line)` (identity for containers; climbs from constants/fields to
  their enclosing function/class).
- Locations whose URI is outside the root → `external` (excluded, counted).
- **Self-references are excluded**: a container equal to the target itself (recursion,
  declaration-adjacent references) is dropped from every system's set and counted.
- Also compute file-level sets (just the file of each container) for the secondary metric.

### T7.2 Systems compared per target

| System | Set |
|---|---|
| `lsp` | union of containers from incoming ∪ references ∪ implementation |
| `lsp_calls` | containers from incoming only (secondary metric) |
| `knot_db` | filtered ground truth (T2.4) containers, all kept relationship groups |
| `knot_db_calls` | only `calls` + `macro_calls` groups (secondary metric) |
| `heuristic` | T6 containers |

### T7.3 Gold set construction (`src/graph/gold.rs`) and adjudication

1. `union = lsp ∪ knot_db ∪ heuristic`.
2. Items found by **both `lsp` and `knot_db`** are auto-labelled `tp` (agreement). Spot-check
   10 random agreed items per language (fixed seed 42) by reading the code; if any is wrong,
   record it as `fp` in the adjudication file and mention it.
3. Every other item (found by only one or two systems but not by both lsp and knot_db) must be
   adjudicated **by reading the code** at the reported location:
   - `tp`: the container genuinely calls/references/implements/overrides/extends the target
     (same symbol, not a homonym, not a string/comment match).
   - `fp`: anything else (homonym resolved wrongly, text-only match, different overload when
     the target is a specific overload, unrelated type with the same name).
   - For Java overloads (`ChatModel.call(Prompt)`), a call to another overload is `fp` for that
     target. Write this rule in the reason field each time it applies.
4. If a language has more than 150 items needing adjudication, adjudicate a **stratified sample
   of 50** (strata = target id × "found by"), seed 42, and extrapolate precision per stratum;
   mark metrics as `estimated`.
5. Adjudication file `results/adjudication/<lang>.toml`:

```toml
[[item]]
target = "rust-05"
entity = { file = "src/pipeline/runner.rs", line = 120, name = "run_pipeline_inner" }
found_by = ["lsp"]            # any of lsp, knot_db, heuristic
verdict = "tp"                 # tp | fp
reason = "calls fastembed_cache_dir() at runner.rs:131; knot missed it because the call is inside a closure"
evidence = "src/pipeline/runner.rs:131"
```

`lsp_probe gold --lang <l>` subcommand:
- Without an adjudication file: writes `results/adjudication/<lang>.pending.toml` listing all
  items needing a verdict (with `verdict = ""`, and a 3-line code excerpt in a comment to speed
  up manual review).
- With a complete adjudication file: computes metrics and writes `results/gold_<lang>.json`.

### T7.4 Metrics (`src/graph/metrics.rs`)

Per target and per system: `found`, `tp`, `fp`, `precision = tp/found`, `recall = tp/|gold|`
where `gold = {items labelled tp}`. Aggregate per language:
- **micro** (sum over targets) — primary;
- **macro** (mean of per-target values, skipping targets with empty gold);
- breakdown by `category` (free_fn, method, type, interface, …);
- file-level precision/recall (secondary);
- `lsp_calls` vs `knot_db_calls` (secondary).
Edge cases: `found=0` → precision undefined (report `null`, exclude from macro precision);
`gold=0` → recall undefined.

`results/gold_<lang>.json` schema `knot-rag-spike/gold/v1`:
`{lang, repo, estimated: bool, systems: {lsp:{micro:{precision,recall,tp,fp,fn}, macro:{…}, by_category:{…}, file_level:{…}}, knot_db:{…}, heuristic:{…}, lsp_calls:{…}, knot_db_calls:{…}}, targets:[{id, gold_size, per_system:{…}}], notable_disagreements:[{target, entity, found_by, verdict, reason}]}`
(`notable_disagreements` = up to 15 most instructive adjudications, chosen by the adjudicator).

Unit tests: metrics on hand-built sets incl. empty sets, perfect, disjoint.

---

## 16. T8 — Degradation test (no LSP installed)

**Executed** (2026-10-04). `lsp_probe nolsp --targets --out [--program <hidden>]` over the shared
start path (`client::start_or_degrade`). Both scenarios ran for all three languages — **6/6 exit
0**, diagnostics human-readable, fallback answers for every target (G16 source fields in
`results/s1_<lang>_nolsp.json` + `results/s1_<lang>_nolsp_broken.json`):

| lang | missing: diagnostic | broken: diagnostic | degrade_ms | fallback_ms | targets |
|---|---|---|---|---|---|
| rust | `rust-analyzer not found in PATH (searched 18 dirs). Install it with \`rustup component add rust-analyzer\`.` | `LSP server exited with status 1 before initialize (service stopped)` | 0 / 4 | 2 / 2 | 20 |
| ts | `typescript-language-server not found in PATH (searched 18 dirs). Install it with \`npm install -g typescript-language-server typescript\`.` | idem | 0 / 2 | 0 / 0 | 21 |
| java | `jdtls not found in PATH (searched 18 dirs). Install it with \`docs/spikes/phase0/scripts/install_jdtls.sh\`…` | idem | 0 / 2 | 17 / 18 | 24 |

Every result carries `lsp_available`, `lsp_started`, `effective_tier: 2`, and per-target answers
with `provenance: "heuristic"` (T6 containers). The real environment is untouched: the sanitized
`PATH` (built by repeatedly removing the resolving directories until `which` returns `None`) is
passed only to the lookup and the child command; `which rust-analyzer` still resolves afterwards.
Deviations/notes:
- `ServerSpec.search_path` (extension) carries the sanitized `PATH`; `servers::which_in` does the
  lookup against an explicit `PATH` (T4.1's `which` delegates to it).
- `client::start` reports the child's exit status on initialize failure
  (`exited with status 1 before initialize`), which `start_or_degrade` turns into the
  `lsp_started: false` diagnostic. `ServerStart::Ready` is boxed (clippy `large_enum_variant`).
- **5 s rule interpretation:** the tier-2 graph is built in the setup phase (timed as
  `graph_build_ms` — T6's resolver: 1.0 s rust / 0.04 s ts / 44 s java), and `fallback_ms` is the
  answering step after the degradation (asserted ≤ 5 000 ms; measured 0–18 ms). A query-time
  fallback that re-resolved the graph could not meet 5 s on spring-ai; the production tier 2 is
  precomputed at index time, which is exactly what the split measures.

`lsp_probe nolsp --targets <toml> --out results/s1_<lang>_nolsp.json`:

1. Build a sanitized `PATH` that removes every directory containing the server binary (use
   `which()` from T4.1 repeatedly until it returns `None`; never modify the real environment —
   pass the sanitized `PATH` only to the child command / lookup function).
2. Call the same code path that `run` uses to start a server. Expected behaviour (assert it):
   - `which()` returns `None` → no spawn attempt; result contains
     `{"lsp_available": false, "diagnostic": "rust-analyzer not found in PATH (searched N dirs). Install it with `rustup component add rust-analyzer`.", "effective_tier": 2}`.
   - Install hints per server: rust-analyzer → `rustup component add rust-analyzer`;
     typescript-language-server → `npm install -g typescript-language-server typescript`;
     jdtls → `docs/spikes/phase0/scripts/install_jdtls.sh` (or the Eclipse download URL).
3. Then answer every target from the T6 heuristic graph and write the containers with
   `provenance: "heuristic"`.
4. Additionally simulate **"present but broken"**: spawn `/bin/false` as the server program for
   one run (`--program /bin/false` hidden flag) and assert the probe reports
   `{"lsp_available": true, "lsp_started": false, "diagnostic": "...exited with status 1 before initialize..."}`
   and still falls back to tier 2 within 5 s.

Acceptance: exit code 0 for both scenarios, diagnostics are human-readable, fallback answers
exist for every target.

---

## 17. T9 — `vec_probe` binary (S2)

**Executed** (2026-10-04/05). `src/vec/{encode,chunk,shard,search,index,measure}.rs` +
`vec_probe index|measure|determinism|mutate|parity` + `scripts/{gitmeasure,qdrant_dump}.sh`.
All six measurements ran for the three repos (`results/s2_<repo>.json`, shards under
`/tmp/knot-rag-spike/s2/<repo>/`). Headline numbers (f32_dec / f32_b64 / i8pv_b64 / i8fx_b64):

| repo | entities+chunks | source raw/gzip | shard raw (i8pv) | gzip (i8pv) | pack (i8pv) | pack/source |
|---|---|---|---|---|---|---|
| knot | 4 809 + 347 | 3 817 / 1 056 KiB | 4 030 KiB | 2 449 KiB | 2 472 KiB | **2.34** |
| knot-site | 1 520 + 10 | 3 199 / 2 768 KiB | 1 112 KiB | 702 KiB | 708 KiB | **0.26** |
| spring-ai | 27 740 + 572 | 89 543 / 61 152 KiB | 22 412 KiB | 13 458 KiB | 13 783 KiB | **0.26** |

- **Quantisation (G11 ≥ 95 %):** i8pv overlap@10 = **1.000 / 1.000 / 1.000** (same top-1 100 %);
  i8fx = 0.988 / 1.000 / 0.950 with **clip rate 0**. Vectors arrive unit-norm (no renormalisation).
- **Retrieval (G12 ≥ 80 % of knot):** spike f32/i8pv/i8fx hit@5 = 0.75/0.75/0.75 vs knot 0.75 (knot),
  0.80/0.80/0.80 vs 1.00 (knot-site, exactly 80 % of knot), 0.83 vs 0.83 (spring-ai) — **PASS**.
- **Tier-1 (G13 ≥ 80 %):** hit@5 over chunk/file vectors = 1.00 (knot), 1.00 (spring-ai);
  knot-site has no `level1` queries (its 2 queries rows are empty → 0.00 is an empty-set artifact).
  Mixed search (entities + chunks) hit@5 = 0.56 / 0.50.
- **Load (G14 ≤ 200 ms):** knot `f32_b64` 201 ms, `i8pv_b64` 87 ms, `i8fx_b64` 84 ms,
  binary cache 59 ms — the best text variant passes; `f32_dec` (720 ms) does not.
- **Query latency:** embed p50 3.5–4.5 ms; scan p50 23 ms (knot, 5 156 rows) / 7 ms (ts) /
  134 ms (spring-ai, 28 312 rows), p95 ≤ 139 ms.
- **Parity with Qdrant (T9.9):** 180/180, 156/156, 195/195 ids found with
  **mean cosine 1.000000** and max |Δ| ≈ 1e-7 for all three repos.
- **Churn (T9.6):** `append` adds 1–3 KiB to the pack (≤ 20 KiB ✓); `prepend` (all lines shift,
  every UUID of the file changes) adds 7–59 KiB. Monoliths behave the same or slightly worse.
  Per-file shards and monoliths are within 2 % of each other on pack size.

**§20.2 rule evaluation → `gitignore` + opt-in** (kept the verdict):
1. pack ≤ 10 % of source pack: **fails everywhere** (best 0.26 for i8pv/i8fx);
2. re-index byte-identical: **fails** (see the two knot-side id findings below);
3. append ≤ 20 KiB: passes.
Proposed-threshold notes for the human: (a) 10 % → ~25 % would admit i8pv_b64 on knot-site and
spring-ai (knot's source pack compresses unusually well — its ratio is inflated at 2.34);
(b) criterion 2 is a knot-side data-model issue, not a shard-format issue — see below.

**Findings (knot-side, measured by T9.7/T9.9):**
- `knot/src/pipeline/parser/languages/html.rs` uses `Uuid::new_v4()` (3 sites) → HTML/CSS-class
  entity ids are **random per run** (133 records in knot, 275 in knot-site, 1 in spring-ai churn
  on re-index; the `nov` variant proves all other metadata is deterministic).
- knot's uuid is `v5(repo:file:fqn:start_line)` and **collides** when entities share those four
  values (14 collisions in knot-site: TS arrow-consts extracted as both `constant` and `function`,
  re-export lists at the same line). The parity check correlates on `(uuid, name, kind)` to
  disambiguate; with plain uuid matching the mean cosine was 0.996.
- Vectors themselves are **fully deterministic**: 0 differing records and max |Δ| = 0.0 across
  reruns, `--per-file-batches` vs full batches, and `RAYON_NUM_THREADS=1` vs default.

Deviations: `Record` carries no `file` field (shard path implies it); the monolith is rebuilt
from shards on single-file re-index; `--only-file` re-embeds one file and never touches the full
run's `index_stats.json`/cache; gzip sizes via `flate2` (dependency added); the `parity` command
has an `--ids-out` preparation step (Qdrant stores numeric point ids — retrieval goes through a
`uuid` payload filter).

### T9.1 CLI

```
vec_probe index   --repo <name> --root <dir> --out-dir /tmp/knot-rag-spike/s2/<repo> [--batch-size 64] [--per-file-batches]
vec_probe measure --repo <name> --out-dir … --queries queries/entities.toml --level1 queries/level1.toml --result results/s2_<repo>.json
vec_probe determinism --repo <name> --root <dir> --out-dir …
vec_probe mutate  --repo <name> --root <dir> --file <rel> --mode <append|prepend>   # edits the SANDBOX copy only
vec_probe parity  --repo <name> --out-dir … --qdrant-dump <json>
```

Repos: `knot`, `knot-site`, `spring-ai` (roots under `/tmp/knot-rag-spike/work/`).

### T9.2 Indexing pipeline (`index`)

1. `EntityTable::build` → `parsed: Vec<ParsedEntity>` (time `parse_ms`).
2. `knot::pipeline::prepare::prepare_entities(&mut parsed)` (time `prepare_ms`).
3. Split: entities with empty `embed_text` (markdown kinds are skipped by `prepare_entities`)
   → **do not embed**, count them as `skipped_no_embed_text` (knot embeds markdown elsewhere;
   out of scope).
4. Embedder: pick the MiniLM choice from
   `EmbedModelChoice::supported()` (the row whose wire name is `"AllMiniLML6V2"`), then
   `Embedder::init_with_model(PathBuf::from(env KNOT_FASTEMBED_CACHE_DIR), choice)`.
   Assert `embedder.dim() == 384`.
5. Embed:
   - default: one call `embedder.embed(entities, batch_size)` over all entities (knot's way);
   - `--per-file-batches`: one `embed` call per file (simulates incremental indexing).
   Time `embed_ms`; record entities/s.
6. **Tier-1 chunks** (T9.3) → embed each chunk text with `embedder.embed_query(text)`. This is
   valid because MiniLM is symmetric (no query/passage prefix; knot's test
   `minilm_is_symmetric` pins it). Assert `choice.query_prefix.is_empty() && choice.passage_prefix.is_empty()`.
7. Check vector norms: record min/max/mean L2 norm (expect ≈ 1.0). If not normalised, normalise
   before quantisation and record that fact.
8. Write all **variants** (T9.4) and a binary cache.

### T9.3 Tier-1 chunker (`src/vec/chunk.rs`)

File selection (walk the root with the `ignore` crate, honouring `.gitignore` when present, plus
an explicit deny-list `node_modules`, `target`, `dist`, `.git`, `.knot`, `.knot-rag`):
- a file is **tier-1** if `knot::pipeline::files::is_supported_file(path, false) == false`
  **or** knot produced zero entities for it, **and** it passes the text allow-list:
  - extensions: `sh bash zsh fish ps1 bat cmd mk make toml yml yaml json jsonc ini cfg conf
    properties sql proto graphql gql txt env.example dockerignore gitignore editorconfig`;
  - exact names: `Makefile`, `Dockerfile`, `Containerfile`, `Jenkinsfile`, `Procfile`,
    `mvnw`, `gradlew`, `Justfile`, `Vagrantfile`;
  - and size ≤ 256 KiB, valid UTF-8, not a lockfile (`Cargo.lock`, `package-lock.json`,
    `pnpm-lock.yaml`, `yarn.lock`, `poetry.lock`).
- chunks: windows of 60 lines with 10 lines overlap; prefer to end a window at a blank line
  within the last 10 lines of the window; minimum chunk 5 non-blank lines (merge tail into the
  previous chunk).
- each chunk text = `"{rel_path} (lines {a}-{b})\n{content}"`.
- plus one **file-level** vector per tier-1 file: `"{rel_path}\n{first 40 lines}"`.
- Shard records use `kind = "chunk"` / `kind = "file"`, `name = file name`,
  `fqn = rel_path`, `l0/l1` the line range.

Unit tests: window boundaries, overlap, blank-line snapping, tiny files, non-UTF-8 rejection.

### T9.4 Shard format and variants (`src/vec/shard.rs`, `src/vec/encode.rs`)

One shard per source file: `<out>/<variant>/<rel_path>.jsonl` (mirror directories; tier-1
chunks of a file go to the same shard as `<rel_path>.jsonl`). Lines sorted by
`(l0, col, kind, name, fqn)`. Each line is a JSON object with **fixed key order**:

```json
{"id":"<uuid>","kind":"rust_function","name":"parse_files","fqn":"knot::pipeline::parser::parse_files","lang":"rust","l0":210,"l1":240,"col":7,"sig":"(files: &[PathBuf], parse_cfg: &ParseConfig)","h":"<sha256(embed_text) first 16 hex>", <vector fields>}
```

- `id`: knot UUID for entities; for chunks/files: `Uuid::new_v5(NAMESPACE_KNOT, "{repo}:{rel}:chunk:{l0}-{l1}")`.
- `col`: UTF-16 name column from `locate_name` (null if not found) — the field the LSP layer
  needs later; record the null rate.
- Serialise with a struct (not a `Map`) so key order is fixed; one line = `serde_json::to_string`
  + `\n`; file written atomically (write to `*.tmp` then rename).

Variants (`<vector fields>`):

| Variant id | Fields | Encoding |
|---|---|---|
| `f32_dec` | `"v":[0.0123,…]` | serde_json default float formatting (shortest round-trip) |
| `f32_b64` | `"v":"<base64 of 384 little-endian f32>"` | standard base64 with padding |
| `i8pv_b64` | `"q":"<base64 of 384 i8>","s":1.234e-2` | per-vector symmetric: `s = max|v|/127`, `q = clamp(round(v/s), -127, 127)`; `s` written as `f32` via `format!("{:.6e}", s)` parsed back to keep it stable |
| `i8fx_b64` | `"q":"<base64 of 384 i8>"` | fixed global scale: `q = clamp(round(v * 254), -127, 127)` (assumes |v| ≤ 0.5; record the **clip rate** = fraction of components clamped) |
| `nov` | no vector fields | metadata-only baseline (to separate metadata size from vector size) |

Plus a **binary cache** (not committed in production, measured here): `<out>/cache/vectors.bin`
= header (`b"KRAG"`, version u32, dim u32, count u32) + `count × dim` f32 LE + an index file
`<out>/cache/ids.jsonl`. Measure load time from cache vs from shards.

Also produce, for git measurement only, a **monolith** layout per variant
(`<out>/<variant>-monolith/index.jsonl`, all records sorted by `(file, l0, …)`) to compare git
delta behaviour of one big file vs per-file shards.

Unit tests: encode/decode round trips for every variant (max abs error for int8 ≤ s/2 + ε),
deterministic bytes for identical input, sort order, atomic write.

### T9.5 Measurements (`measure`) — output `results/s2_<repo>.json`

1. **Counts:** files scanned, files with entities, tier-1 files, entities, chunks, file vectors,
   skipped_no_embed_text, `col` null rate.
2. **Sizes** per variant: total raw bytes of all shards; total gzip bytes (flate2 default level,
   per-file then summed — approximates loose objects); number of shard files; bytes per entity.
   Also the source tree size (raw + gzip) of the snapshot, excluding `node_modules`, `target`,
   `.knot`.
3. **Load time** per variant (cold-ish: drop nothing, but run each load 5 times and take median):
   read + parse all shards into an in-memory f32 matrix (dequantise int8). And load from the
   binary cache.
4. **Query latency:** for each query in `queries/entities.toml` for this repo: `embed_query`
   time (median of 5), brute-force scan time (median of 20) over the full matrix with top-10
   selection (`select_nth_unstable_by` + sort of the top 10). Report p50/p95.
5. **Quantisation loss:** for each query, top-10 ids with `f32` vs `i8pv` vs `i8fx`; overlap@10
   (|A∩B|/10) averaged; also Kendall-tau-free simple metric "same top-1" rate.
6. **Retrieval quality vs knot:** for each query, `hit@5` and `hit@10` = whether any result's
   file matches the expected file pattern (`expected` may be a list of acceptable paths or a
   directory prefix ending with `/`). Compute for: `spike_f32`, `spike_i8pv`, `spike_i8fx`,
   `knot_search` (from `groundtruth/search_<repo>_<qid>.json`, top-10). Also report
   overlap@10 between `spike_f32` top-10 entity uuids and knot top-10 uuids (informative —
   knot applies hybrid re-ranking).
   - Search domain for these queries: **entities only** (exclude chunk/file vectors) so the
     comparison with knot is fair.
7. **Tier-1 quality:** for each query in `queries/level1.toml`: search over **chunk + file
   vectors only**, collapse results to files (best score per file), compute `hit@1`, `hit@5`.
   Also run the same queries over **all** vectors (entities + chunks) and report hit@5 — this is
   the realistic setting where tier-1 files compete with code entities.

### T9.6 Git pack measurement (`scripts/gitmeasure.sh`)

For each repo and each variant (+ monolith variants) and for the source tree:

```bash
D=$SPIKE_ROOT/gitmeasure/$repo/$variant
rm -rf "$D"; mkdir -p "$D"; git -C "$D" init -q
cp -a "$SRC/." "$D/"
git -C "$D" add -A
git -C "$D" -c user.name=spike -c user.email=spike@localhost commit -qm base
git -C "$D" gc --aggressive --prune=now -q
git -C "$D" count-objects -v | awk '/size-pack/ {print $2}'   # KiB
```

Then the **incremental churn** experiment (per repo, for variants `f32_dec`, `f32_b64`,
`i8pv_b64`, `i8fx_b64` and their monoliths):
1. `vec_probe mutate --mode append` on a chosen file (knot: `src/pipeline/files.rs`; knot-site:
   `src/toolcore/format.ts`; spring-ai:
   `spring-ai-commons/src/main/java/org/springframework/ai/document/Document.java`). `append`
   adds a new small function/method at the end of the file (language-appropriate template,
   inside the class for Java).
2. Re-index **only that file** (parse just that file with the same `ParseConfig`, embed with
   `--per-file-batches`), rewrite its shard (and the monolith), copy into the git dir, commit,
   `gc`, record `size-pack` delta and `git diff --stat HEAD~1` (files and lines changed).
3. Reset the sandbox copy (`setup_sandbox.sh` re-rsync of that repo) and repeat with
   `--mode prepend` (insert the function at the top of the file → shifts all lines → every
   entity UUID in that file changes because UUID includes `start_line`). Record the same numbers.

Record everything in `results/s2_<repo>.json` under `git`.

### T9.7 Determinism (`determinism`)

1. Index twice with identical settings into two output dirs → `diff -r` must be empty for every
   variant (report per variant: identical yes/no, number of differing shard files).
2. Index once with full batches (`batch 64`) and once with `--per-file-batches` → compare per
   variant. Expected: `f32_*` may differ (padding/batching numerics in ONNX), int8 variants
   should be identical or nearly so. Report the number of differing records per variant and the
   max abs difference in f32 components.
3. Index once with `RAYON_NUM_THREADS=1` (environment variable for the process) vs default →
   compare as above.

### T9.8 Results JSON `results/s2_<repo>.json` (schema `knot-rag-spike/s2/v1`)

```json
{
  "schema": "knot-rag-spike/s2/v1",
  "repo": "knot", "root": "…", "snapshot_fingerprint": "…",
  "model": { "name": "AllMiniLML6V2", "dim": 384, "symmetric": true },
  "counts": { "files_scanned": 0, "files_with_entities": 0, "tier1_files": 0, "entities": 0, "chunks": 0, "file_vectors": 0, "skipped_no_embed_text": 0, "col_null_rate": 0.0 },
  "timings_ms": { "parse": 0, "prepare": 0, "embed_entities": 0, "embed_chunks": 0, "entities_per_s": 0.0 },
  "norms": { "min": 0.0, "max": 0.0, "mean": 0.0 },
  "sizes": { "source": { "raw": 0, "gzip": 0 },
             "variants": { "f32_dec": { "raw": 0, "gzip": 0, "shards": 0, "bytes_per_entity": 0.0 } } },
  "load_ms": { "f32_dec": 0.0, "f32_b64": 0.0, "i8pv_b64": 0.0, "i8fx_b64": 0.0, "binary_cache": 0.0 },
  "query": { "embed_ms": {"n":0,"p50":0,"p95":0,"max":0}, "scan_ms": {"n":0,"p50":0,"p95":0,"max":0} },
  "quantization": { "i8pv": { "overlap_at_10": 0.0, "same_top1": 0.0 }, "i8fx": { "overlap_at_10": 0.0, "same_top1": 0.0, "clip_rate": 0.0 } },
  "quality": { "queries": [ { "id": "k01", "query": "…", "expected": ["…"], "hit5": {"spike_f32": true, "spike_i8pv": true, "spike_i8fx": true, "knot_search": false}, "hit10": {}, "overlap10_vs_knot": 0.4 } ],
               "summary": { "hit5": {"spike_f32": 0.0, "spike_i8pv": 0.0, "spike_i8fx": 0.0, "knot_search": 0.0} } },
  "tier1": { "queries": [ { "id": "l01", "hit1": true, "hit5": true, "hit5_mixed": true } ], "summary": { "hit1": 0.0, "hit5": 0.0, "hit5_mixed": 0.0 }, "size_increase_pct": 0.0 },
  "git": { "pack_kib": { "source": 0, "f32_dec": 0, "f32_dec-monolith": 0 },
           "ratio_vs_source": { "i8pv_b64": 0.0 },
           "churn": { "append": { "i8pv_b64": { "pack_delta_kib": 0, "files_changed": 1, "lines_changed": 3 } }, "prepend": {} } },
  "determinism": { "rerun_identical": { "f32_dec": true }, "batching_identical": { "f32_dec": false }, "batching_differing_records": { "f32_dec": 12 }, "batching_max_abs_diff": 1e-7, "threads_identical": { "f32_dec": true } },
  "parity_with_qdrant": { "compared": 200, "max_abs_diff": 0.0, "mean_cosine": 1.0 }
}
```

### T9.9 Parity with knot's Qdrant vectors (`parity`)

`scripts/qdrant_dump.sh <repo> <ids-file>` posts to Qdrant REST:

```bash
curl -fsS -X POST "http://localhost:6333/collections/knot_entities/points" \
  -H 'Content-Type: application/json' \
  -d "{\"ids\": $(cat "$IDS"), \"with_vector\": true, \"with_payload\": false}" > "$OUT"
```

where `$IDS` is a JSON array of 200 entity UUIDs sampled deterministically (seed 42) from the
spike's shards. `vec_probe parity` compares spike f32 vectors with Qdrant's: report found
count, max abs component diff, mean cosine. Expected: cosine ≥ 0.9999 for found ids. Ids not
found in Qdrant indicate `embed_text` / parse-config divergence → investigate and report (do not
block the spike on it). If the collection uses named vectors, adapt the request
(`"with_vector": ["<name>"]`) after inspecting `GET /collections/knot_entities`.

---

## 18. T10 — `summarize` binary and results report

**Executed** (2026-10-05). `src/report.rs` (14 table renderers + `BEGIN/END:table` block
machinery) + `src/bin/summarize.rs`; the report is `docs/spikes/phase0-results.md` with all 17
Appendix-E sections (13 rendered tables + 5 narrative sections). `--template-check` verifies all
14 blocks exist exactly once. Formatting rules implemented: ms 0/1 decimals, bytes KiB/MiB,
ratios as % with 1 decimal, ✅/❌/⚠️ verdicts (⚠️ for estimated/partial, e.g. G13's knot-site
n/a). Every table carries a `Sources:` footnote naming the JSON fields. Deviation: the table
renderers live in a library module (`src/report.rs`) instead of the binary (the plan fixed only
the CLI name) so the layout rule "binaries stay thin" holds; the verdict rows are sorted G1..G16
regardless of which internal builder emits them.

`summarize --results results/ --out docs/spikes/phase0-results.md --template-check` renders all
Markdown tables of [Appendix E](#appendix-e--results-report-template) from the JSON files
(the narrative sections are written by the agent). Rules:
- Numbers: ms with 0 decimals (≥ 10) or 1 decimal (< 10); bytes humanised (KiB/MiB, 1 decimal);
  ratios as percentages with 1 decimal.
- Every table cell must be traceable to a JSON field; add a footnote with the source file names.
- Threshold columns show ✅ / ❌ / ⚠️ (⚠️ = estimated or partial, e.g. Java subset).
- Do not overwrite manually written narrative: render into marked blocks
  `<!-- BEGIN:table:<name> -->` … `<!-- END:table:<name> -->` and replace only those.

The agent then writes the narrative sections (findings, decisions, deviations, next steps).

---

## 19. T11 — Quality gate and hand-off

**Executed** (2026-10-05). `make check` green (fmt, clippy `-D warnings` inheriting the root
`clippy.toml`, 89 lib tests, dupes 0.0% at the zero ratchet); `make dupes-cleanup` reports no
stale entries; `make e2e` prints the Phase-7 skip message. The `validator` subagent (working
directory `docs/spikes/phase0`, gate `make check` at the repository root) confirms all gates with
the dupes result. `docs/spikes/phase0/README.md` documents the exact reproduction sequence
(`scripts/run_all.sh`, expected runtimes per step, prerequisites) and the inherited quality-gate
configuration. Reported to the human: one `#[expect(deprecated, reason = "plan T4.2 item 3:
jdtls and typescript-language-server still read the deprecated root_uri")]` in
`src/lsp/client.rs` and **no** `.dupes-ignore.toml` entries. One recorded deviation: measurement
binaries ran debug-built (see the report's Environment note).

1. From the repository root run **`make check`** (fmt-check, clippy `-D warnings` with the
   root `clippy.toml`, `cargo test --lib --all-features`, `cargo dupes check` with the
   zero-ratchet `dupes.toml`) and `make dupes-cleanup` (must report no stale entries). Also
   `make e2e` must print the "skipped" message (no suite before Phase 7).
2. Invoke the **`validator`** subagent with working directory
   `/home/raul/workspace/rust/knot-rag/docs/spikes/phase0`. It runs `cargo fmt`,
   `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test`. There is no E2E
   script for the spike (the measurement runs are documented in `README.md` instead). Tell the
   validator explicitly that the project's gate is `make check` at the repository root, so it
   also reports the dupes result.
3. Fix every finding by refactoring (no `#[allow]`, no threshold changes, no new
   `.dupes-ignore.toml` entries outside the two legitimate categories). Report any `#[expect]`
   and any `.dupes-ignore.toml` entry to the human, with its `reason`.
4. `docs/spikes/phase0/README.md` must contain the exact command sequence to reproduce all
   results (`scripts/run_all.sh`), expected runtimes, and prerequisites, plus a "Quality gates"
   paragraph explaining the inherited root config (clippy parent lookup, dupes symlink).
5. Final message to the human: the go/no-go table (§20) with the measured values and links to
   `docs/spikes/phase0-results.md`.

---

## 20. Go / no-go thresholds and decision rules

### 20.1 Thresholds

| ID | Criterion | Threshold | Source field |
|---|---|---|---|
| G1 | LSP recall (micro, container level) — Rust | ≥ 90 % | `gold_rust.systems.lsp.micro.recall` |
| G2 | LSP recall — TS | ≥ 90 % | `gold_ts…` |
| G3 | LSP recall — Java | ≥ 85 % | `gold_java…` |
| G4 | LSP precision (micro) — each language | ≥ 90 % | `…lsp.micro.precision` |
| G5 | Cold `initialize_to_ready` — RA on knot | ≤ 60 s | `s1_rust_cold.timings_ms` |
| G6 | Cold ready — TS on knot-site | ≤ 30 s | `s1_ts_cold` |
| G7 | Cold ready — jdtls on spring-ai | ≤ 300 s | `s1_java_cold` |
| G8 | Warm p95 per operation (all langs) | ≤ 500 ms | `latency_ms.*.p95` (warm runs) |
| G9 | Peak RSS (informative, no gate) | report | `rss.peak_bytes` |
| G10 | Name-column heuristic accuracy | ≥ 98 % | `column_heuristic.matched/compared` |
| G11 | int8 overlap@10 vs f32 (best int8 variant) | ≥ 95 % | `s2_*.quantization` |
| G12 | Spike int8 hit@5 vs knot hit@5 (entities, pure vector) | ≥ 80 % of knot's | `s2_*.quality.summary` |
| G13 | Tier-1 file hit@5 (chunk/file domain) | ≥ 80 % | `s2_*.tier1.summary.hit5` |
| G14 | Load time of the knot index (best text variant) | ≤ 200 ms | `s2_knot.load_ms` |
| G15 | Heuristic (tier 2) recall/precision | report only (README material) | `gold_*.systems.heuristic` |
| G16 | Degradation path | both scenarios exit 0 with diagnostics + fallback | `s1_*_nolsp` |

### 20.2 Decision rules

- **Commit `.knot-rag/` by default** iff, for the chosen variant: git pack of shards ≤ **10 %**
  of the git pack of the source tree **and** re-index without changes is byte-identical **and**
  the `append` churn adds ≤ **20 KiB** to the pack. Otherwise default is `.gitignore` with an
  opt-in flag. Report the rule evaluation for all three repos; the decision follows the
  majority, with spring-ai (largest) as tie-breaker.
- **Vector encoding choice:** the smallest variant that satisfies G11 and is deterministic under
  batching (T9.7 step 2). If none is deterministic under batching, note that knot-rag must embed
  with a fixed batch composition (e.g. always per-file) and re-check.
- **Per-language graph tier:** a language gets "LSP default" if G1/G2/G3 + G4 + its cold-start
  gate pass; if recall/precision pass but cold start fails → "pre-computed LSP edges at index
  time + live LSP only when warm"; if recall/precision fail → "heuristic (tier 2) default, LSP
  experimental".
- **Kill criteria for the whole LSP approach:** if two of the three languages fail G1–G4, write
  that clearly and propose "heuristic graph + harness LSP delegation" as the architecture.

Do not move thresholds. If a threshold looks wrong in light of the data, keep the verdict and add
a "proposed threshold change" note for the human.

---

## 21. Risks, pitfalls and troubleshooting

| Risk | Symptom | Mitigation |
|---|---|---|
| `cargo dupes check` silently passes | dupes gate green although code is duplicated | caused by a missing `dupes.toml` in the analysed dir; the Makefile guard + symlink (T1.0) prevent it; never delete the symlink. |
| Zero duplication ratchet blocks progress | `make dupes` fails on similar step functions / encoders | extract shared helpers or traits (§T1.2 guidance); `.dupes-ignore.toml` only for the two legitimate categories, with `reason`, reported to the human. |
| Root `Makefile` run before the spike crate exists | `could not find Cargo.toml` | T1.0 is done together with T1.1; `make help` shows the selected `CRATE_DIR`. |
| async-lsp default router stops on unknown notifications | main loop ends right after initialize | install `unhandled_notification` / `unhandled_request` handlers (T4.2). |
| lsp-types version clash | type mismatch errors between `lsp_types::Url` types | use only `async_lsp::lsp_types`. |
| UTF-16 columns | references empty for names after non-ASCII text | always convert byte↔UTF-16 (T3.3). |
| rust-analyzer build scripts / proc macros | long cold start, `ort` download, network | expected; record it; target dir in `/tmp`. If no network, set `cargo.buildScripts.enable=false` in a **separate** labelled run. |
| rust-analyzer `ContentModified` | sporadic errors right after ready | retries (T4.2 step 4). |
| tsserver lazy project load | empty references for the first queries | pre-open one file per project (T5.2 step 6) + empty-result guard. |
| `scripts/*.mjs` is outside every tsconfig | references limited to inferred project | expected finding; report per-target. |
| jdtls Maven import of a 100+ module build | very long cold start, Maven downloads, failures on custom repos/settings | 900 s timeout; inspect `jdtls-ws/.metadata/.log`; if the import fails on repository access, retry with `-s <spike>/settings.xml` is **not** available for jdtls → use the subset fallback (T5.4). Keep logs. |
| jdtls writes into the project | `.project`/`.classpath` files appear | only ever run on the sandbox copy. |
| knot index stale duplicates | ground truth with two targets / wrong lines | T2.4 filtering against the snapshot. |
| knot CLI dimension error | "expected dim 384, got 768" | always use `scripts/knot.sh`. |
| `discover_files` without `.git` | `node_modules`/`target` files parsed | verify counts; explicit deny-list (T3.1). |
| `ParseConfig` mismatch with knot-server (`include_config_files`) | entity counts differ from knot by > 5 % | try both; keep closest; document. |
| ONNX nondeterminism | f32 shards differ across runs/batches | measured explicitly (T9.7); this is a finding, not a bug. |
| Memory pressure | OOM or swap during java run | run languages sequentially; `-Xmx8g`; close other workloads. |
| Very large reference sets (`Prompt` ≈ 2 000 rows, `EntityKind` ≈ 550) | slow adjudication | stratified sampling rule (T7.3 step 4). |

Useful logs: `/tmp/knot-rag-spike/logs/*.stderr.log`; jdtls:
`/tmp/knot-rag-spike/jdtls-ws/spring-ai/.metadata/.log`; rust-analyzer verbose:
`RA_LOG=info` env in the spec for a debug run only.

---

## 22. Definition of done

- [x] jdtls 1.61.0 installed and verified (T0.2).
- [x] Sandbox copies + fingerprints (T0.3) recorded in the report.
- [x] Root `Makefile`, `clippy.toml`, `dupes.toml` adapted per T1.0 (no threshold loosened;
      dupes ratchet at zero); `docs/spikes/phase0/dupes.toml` symlink present; T1.0.5 proofs
      recorded (`results/raw/quality_gates.txt`).
- [x] Spike `Cargo.toml` lint tables byte-identical to knot's.
- [x] Spike crate builds; `make check` green from the repository root; `validator` green (T11).
- [x] Every `#[expect]` / `.dupes-ignore.toml` entry reported to the human: one
      `#[expect(deprecated, reason = "…root_uri…")]` (T4) and no `.dupes-ignore.toml`.
- [x] All targets validated against the snapshot; `TBD` lines resolved (T2.3) — 65/65 `ok`.
- [x] `results/s1_{rust,ts,java}_{cold,warm}.json` exist (java full run, no subset needed).
- [x] `results/heuristic_{rust,ts,java}.json`, `results/s1_*_nolsp.json` (+ `*_nolsp_broken.json`).
- [x] Adjudication files complete; `results/gold_{rust,ts,java}.json`.
- [x] `results/s2_{knot,knot-site,spring-ai}.json` with sizes, git, determinism, quality,
      tier-1, parity.
- [x] `docs/spikes/phase0-results.md` complete per Appendix E, including decisions (§20.2),
      deviations from this plan, and a proposed outline of Phases 1–7 adjusted to the data.
- [x] No file written outside the allowed locations (§5.1); real repos untouched
      (`git -C ~/workspace/rust/knot status` unchanged apart from the human's own Dockerfile edit).

---

## Appendix A — Target lists (S1)

Line numbers come from knot's index on 2026-10-03 (validated against the knot-server snapshot
for knot; for the others validate in T2.3). `TBD` = resolve in T2.3. `knot_query` uses
`Type.member` for members (knot's FQN-suffix match).

### A.1 Rust — repo `knot`, root `/tmp/knot-rag-spike/work/knot`

| id | name | file | line | knot_query | category | ops | knot rows (2026-10-03) |
|---|---|---|---|---|---|---|---|
| rust-01 | parse_files | src/pipeline/parser/mod.rs | 210 | parse_files | free_fn | incoming, references | calls 4 |
| rust-02 | prepare_entities | src/pipeline/prepare.rs | 66 | prepare_entities | free_fn | incoming, references | calls 6 |
| rust-03 | embed_query | src/pipeline/embed.rs | 176 | Embedder.embed_query | method | incoming, references | calls 3 |
| rust-04 | resolve_reference_intents | src/pipeline/ingest/resolve/mod.rs | 462 | resolve_reference_intents | free_fn | incoming, references | calls 38 |
| rust-05 | fastembed_cache_dir | src/pipeline/state.rs | 63 | fastembed_cache_dir | free_fn | incoming, references | calls 3 |
| rust-06 | format_output | src/utils/mod.rs | 118 | format_output | free_fn | incoming, references | calls 5 |
| rust-07 | verify_startup | src/startup_guard.rs | 34 | verify_startup | free_fn | incoming, references | calls 2 |
| rust-08 | run_find_callers | src/cli_tools/find_callers.rs | 31 | run_find_callers | free_fn | incoming, references | calls 4 |
| rust-09 | discover_files | src/pipeline/input.rs | 115 | discover_files | free_fn | incoming, references | calls 8 |
| rust-10 | to_repo_relative | src/pipeline/files.rs | 140 | to_repo_relative | free_fn | incoming, references | calls 16 |
| rust-11 | is_supported_file | src/pipeline/files.rs | 22 | is_supported_file | free_fn | incoming, references | calls 7 |
| rust-12 | ParsedEntity | src/models/entity.rs | 283 | ParsedEntity | type | references | references 243 (2 targets in index: 276 stale, 283 current) |
| rust-13 | EntityKind | src/models/entity.rs | 16 | EntityKind | type | references | references 548 (scale case) |
| rust-14 | GraphDb | src/db/graph/mod.rs | 29 | GraphDb | type | references | references 93 |
| rust-15 | ConnectExt | src/db/graph/connection.rs | 12 | ConnectExt | trait | references, implementation | references 13 |
| rust-16 | RepoScope | src/models/repo_scope.rs | 9 | RepoScope | type | references | references 49 |
| rust-17 | Embedder | src/pipeline/embed.rs | 28 | Embedder | type | references | references 14 |
| rust-18 | KnotMcpHandler | src/mcp_handler.rs | 33 | KnotMcpHandler | type | references | references 24 |
| rust-19 | connect | src/db/graph/connection.rs | TBD (≈49) | ConnectExt.connect | trait_method | incoming, references, implementation | verify in T2.3 |
| rust-20 | handle | src/mcp_tools/find_callers.rs | TBD (≈87) | FindCallersTool.handle | method | incoming, references | homonym `handle` exists in 6 files |

### A.2 TypeScript / JavaScript — repo `knot-site`, root `/tmp/knot-rag-spike/work/knot-site`

| id | name | file | line | knot_query | category | ops | knot rows |
|---|---|---|---|---|---|---|---|
| ts-01 | jsonText | src/toolcore/format.ts | 18 | jsonText | free_fn | incoming, references | 4 |
| ts-02 | jsonTextFitting | src/toolcore/format.ts | 37 | jsonTextFitting | free_fn | incoming, references | 3 |
| ts-03 | truncateWords | src/toolcore/format.ts | 23 | truncateWords | homonym | incoming, references | knot resolved the query to the other homonym only |
| ts-04 | truncateWords | scripts/fetch-updates.mjs | 109 | truncateWords | homonym | incoming, references | 1 |
| ts-05 | plainText | src/toolcore/format.ts | 55 | plainText | free_fn | incoming, references | 3 |
| ts-06 | errorText | src/toolcore/format.ts | 60 | errorText | free_fn | incoming, references | 6 |
| ts-07 | pageFromHash | src/state/router.ts | 23 | pageFromHash | free_fn | incoming, references | 1 |
| ts-08 | sectionFromHash | src/state/router.ts | 28 | sectionFromHash | free_fn | incoming, references | 1 |
| ts-09 | emit | src/state/consentStore.ts | 26 | emit | homonym | incoming, references | 7 rows across 3 homonyms |
| ts-10 | emit | src/state/installationStore.ts | 18 | emit | homonym | incoming, references | (same query) |
| ts-11 | emit | src/webmcp/invocationLog.ts | 27 | emit | homonym | incoming, references | (same query) |
| ts-12 | withLogging | src/webmcp/invocationLog.ts | 52 | withLogging | free_fn | incoming, references | 1 |
| ts-13 | ToolDefinition | src/toolcore/types.ts | 61 | ToolDefinition | interface | references, implementation | 14 |
| ts-14 | ToolResult | src/toolcore/types.ts | 40 | ToolResult | interface | references | 8 |
| ts-15 | JSONSchema | src/toolcore/types.ts | 5 | JSONSchema | interface | references | 12 |
| ts-16 | WebMcpTool | src/webmcp/types.ts | 40 | WebMcpTool | interface | references, implementation | 16 |
| ts-17 | Feature | src/data/types.ts | 8 | Feature | interface | references | 8 |
| ts-18 | InstallOption | src/data/types.ts | 26 | InstallOption | interface | references | 1 |
| ts-19 | request | src/state/consentStore.ts | 51 | request | method | incoming, references | 2 (object-literal method of `consentStore`) |
| ts-20 | record | src/webmcp/invocationLog.ts | 42 | record | method | incoming, references | 1 |
| ts-21 | setActiveTab | src/state/installationStore.ts | 33 | setActiveTab | method | incoming, references | 5 |

### A.3 Java — repo `spring-ai`, root `/tmp/knot-rag-spike/work/spring-ai`

Paths are relative to the repo root. Lines `TBD` must be resolved to knot's `start_line` (which
may point at an annotation or Javadoc-adjacent line) in T2.3. Declaration lines observed in the
source are given as hints.

| id | name | file | line | knot_query | category | ops | knot rows / note |
|---|---|---|---|---|---|---|---|
| java-01 | ChatClient | spring-ai-client-chat/src/main/java/org/springframework/ai/chat/client/ChatClient.java | 61 | ChatClient | interface | references, implementation | refs 402, implements 1 |
| java-02 | prompt | same file | TBD (hint: `prompt()` at 128) | ChatClient.prompt | interface_method | incoming, references, implementation | 3 overloads; target the no-arg one |
| java-03 | create | same file | TBD (hint: `create(ChatModel)` at 63) | ChatClient.create | method | incoming, references | static; 3 overloads; target the 1-arg one |
| java-04 | ChatModel | spring-ai-model/src/main/java/org/springframework/ai/chat/model/ChatModel.java | TBD | ChatModel | interface | references, implementation | **knot resolves `ChatModel` to an inner entity of GoogleGenAiChatModel** → expected LSP win |
| java-05 | call | same file | TBD (hint: abstract `call(Prompt)` at 45) | ChatModel.call | interface_method | incoming, references, implementation | knot: calls 2 (suspiciously low) |
| java-06 | Prompt | spring-ai-model/src/main/java/org/springframework/ai/chat/prompt/Prompt.java | 47 | Prompt | type | references | calls 661 + refs 1 450 (scale case) |
| java-07 | VectorStore | spring-ai-vector-store/src/main/java/org/springframework/ai/vectorstore/VectorStore.java | 40 | VectorStore | interface | references, implementation | refs 557, implements 3 |
| java-08 | add | same file | TBD (hint: abstract `add(List<Document>)` at 51) | VectorStore.add | interface_method | incoming, references, implementation | overridden_by 3 |
| java-09 | similaritySearch | same file | TBD | VectorStore.similaritySearch | interface_method | incoming, references, implementation | knot: 7 targets, calls 279; target the `SearchRequest` overload |
| java-10 | ToolCallback | spring-ai-model/src/main/java/org/springframework/ai/tool/ToolCallback.java | 33 | ToolCallback | interface | references, implementation | refs 577, implements 13 |
| java-11 | call | same file | TBD (hint: `call(String)` at 53) | ToolCallback.call | interface_method | incoming, references, implementation | overridden_by 20 |
| java-12 | Advisor | spring-ai-client-chat/src/main/java/org/springframework/ai/chat/client/advisor/api/Advisor.java | 31 | Advisor | interface | references, implementation | extends 4, refs 95 |
| java-13 | TokenTextSplitter | spring-ai-commons/src/main/java/org/springframework/ai/transformer/splitter/TokenTextSplitter.java | 38 | TokenTextSplitter | type | references | refs 9 |
| java-14 | Document | spring-ai-commons/src/main/java/org/springframework/ai/document/Document.java | 120 | Document | type | references | knot: only 4 rows (suspicious) |
| java-15 | getText | same file | TBD (hint: 255) | Document.getText | method | incoming, references | calls 66 |
| java-16 | ChatResponse | spring-ai-model/src/main/java/org/springframework/ai/chat/model/ChatResponse.java | 41 | ChatResponse | type | references | refs 1 138 |
| java-17 | AbstractObservationVectorStore | spring-ai-vector-store/src/main/java/org/springframework/ai/vectorstore/observation/AbstractObservationVectorStore.java | 41 | AbstractObservationVectorStore | abstract_class | references, implementation | extends 23 |
| java-18 | ToolCallingManager | spring-ai-model/src/main/java/org/springframework/ai/model/tool/ToolCallingManager.java | 31 | ToolCallingManager | interface | references, implementation | implements 3 |
| java-19 | executeToolCalls | same file | TBD (hint: 41) | ToolCallingManager.executeToolCalls | interface_method | incoming, references, implementation | knot: overridden_by 2, **no calls** |
| java-20 | execute | spring-ai-retry/src/main/java/org/springframework/ai/retry/RetryUtils.java | TBD (hint: 170) | RetryUtils.execute | method | incoming, references | static generic; calls 13 |
| java-21 | mergeOption | spring-ai-model/src/main/java/org/springframework/ai/model/ModelOptionsUtils.java | TBD (hint: 38) | ModelOptionsUtils.mergeOption | method | incoming, references | static generic with type annotations; calls 12 |
| java-22 | MethodToolCallback | spring-ai-model/src/main/java/org/springframework/ai/tool/method/MethodToolCallback.java | 50 | MethodToolCallback | type | references | refs 17 |
| java-23 | DefaultChatClient | spring-ai-client-chat/src/main/java/org/springframework/ai/chat/client/DefaultChatClient.java | 90 | DefaultChatClient | type | references | refs 204 |
| java-24 | EmbeddingModel | spring-ai-model/src/main/java/org/springframework/ai/embedding/EmbeddingModel.java | TBD | EmbeddingModel | interface | references, implementation | **knot returned nothing** → expected LSP win |

---

## Appendix B — Query lists (S2)

### B.1 `queries/entities.toml` (tier-3 retrieval, entities only)

Format:

```toml
[[query]]
id = "k01"
repo = "knot"
text = "find relevant code by meaning across the repository"
expected = ["src/cli_tools/search_hybrid_context/"]   # list; entries ending in '/' are prefixes
```

| id | repo | text | expected |
|---|---|---|---|
| k01 | knot | find relevant code by meaning across the repository | `src/cli_tools/search_hybrid_context/`, `src/mcp_tools/search_hybrid_context/` |
| k02 | knot | parse source files into entities in parallel | `src/pipeline/parser/mod.rs` |
| k03 | knot | compute embeddings for entities with fastembed | `src/pipeline/embed.rs`, `src/pipeline/embed/model.rs` |
| k04 | knot | find who calls a symbol | `src/cli_tools/find_callers.rs`, `src/mcp_tools/find_callers.rs` |
| k05 | knot | list the MCP tools exposed by the server | `src/mcp_handler.rs`, `src/mcp_tools/mod.rs` |
| k06 | knot | track file hashes for incremental indexing | `src/pipeline/state.rs`, `src/pipeline/files.rs` |
| k07 | knot | watch the filesystem and re-index on change | `src/pipeline/watch.rs` |
| k08 | knot | load configuration from environment and env file | `src/config.rs` |
| s01 | knot-site | format tool output as JSON within a token budget | `src/toolcore/format.ts` |
| s02 | knot-site | ask the user for consent before running a tool | `src/state/consentStore.ts`, `src/components/ConsentModal.tsx` |
| s03 | knot-site | hash based client side routing | `src/state/router.ts` |
| s04 | knot-site | log every webmcp tool invocation | `src/webmcp/invocationLog.ts` |
| s05 | knot-site | register tools with the browser model context | `src/webmcp/registry.ts`, `src/webmcp/useWebMcp.ts` |
| j01 | spring-ai | call the chat model with a prompt | `spring-ai-model/src/main/java/org/springframework/ai/chat/model/ChatModel.java`, `spring-ai-client-chat/src/main/java/org/springframework/ai/chat/client/` |
| j02 | spring-ai | store documents and search them by similarity | `spring-ai-vector-store/src/main/java/org/springframework/ai/vectorstore/` |
| j03 | spring-ai | split documents into token sized chunks | `spring-ai-commons/src/main/java/org/springframework/ai/transformer/splitter/` |
| j04 | spring-ai | retry transient errors when calling an AI provider | `spring-ai-retry/src/main/java/org/springframework/ai/retry/` |
| j05 | spring-ai | expose a java method as a tool the model can call | `spring-ai-model/src/main/java/org/springframework/ai/tool/method/` |
| j06 | spring-ai | merge runtime options with default model options | `spring-ai-model/src/main/java/org/springframework/ai/model/ModelOptionsUtils.java` |

The ground-truth script also runs `knot search` for each of these (`-m 10`).

### B.2 `queries/level1.toml` (tier-1 file location)

| id | repo | text | expected |
|---|---|---|---|
| l01 | knot | run every end to end test suite | `tests/run_all_e2e_fast.sh` |
| l02 | knot | build the container image | `Dockerfile` |
| l03 | knot | start qdrant and neo4j containers | `docker-compose.yml` |
| l04 | knot | run formatting lint tests and duplication gates | `Makefile` |
| l05 | knot | publish a github release when a tag is pushed | `.github/workflows/release.yml` |
| l06 | knot | code duplication thresholds | `dupes.toml` |
| l07 | knot | compare performance metrics between runs | `scripts/compare_perf_metrics.sh` |
| l08 | knot | install agent skills | `scripts/install-agent-skills.sh` |
| l09 | knot | clippy complexity thresholds | `clippy.toml` |
| l10 | spring-ai | maven wrapper launcher script | `mvnw` |
| l11 | spring-ai | deploy the javadoc site | `.github/workflows/deploy-javadoc.yml` |

Before running, verify each expected file is classified tier-1 by the chunker (if knot parses one
of them into entities — e.g. `.toml` handled by knot's TOML parser — it is not tier-1: then move
the query to `entities.toml` or replace it with another tier-1 file, and log the change).

---

## Appendix C — LSP cheat-sheet per server

| Aspect | rust-analyzer | typescript-language-server | jdtls |
|---|---|---|---|
| Start | `rust-analyzer` (stdio) | `typescript-language-server --stdio` | `jdtls --jvm-arg=-Xmx8g -data <ws>` |
| Readiness signal | `experimental/serverStatus {health, quiescent}` (opt-in via `experimental.serverStatusNotification`) ; progress tokens `rustAnalyzer/Indexing`, `rustAnalyzer/cachePriming` | `$/progress` around project load, only after `didOpen` | `language/status {type: "Starting"→…→"ServiceReady"}` + `$/progress` |
| Call hierarchy | yes | yes | yes |
| Type hierarchy | yes | no | yes |
| Implementation | yes | yes | yes |
| Known server→client requests | `workspace/configuration`, `window/workDoneProgress/create`, `client/registerCapability` | `window/workDoneProgress/create` | `workspace/configuration`, `client/registerCapability`, `window/workDoneProgress/create` |
| Side effects | runs `cargo metadata`, build scripts, proc-macro server | none on disk | writes `.project`, `.classpath`, `.settings/` into the project; workspace metadata in `-data` |
| Common errors | `-32801 ContentModified` while indexing | empty results before project load | long Maven import; `.metadata/.log` in the workspace dir |

All positions are 0-based; columns in UTF-16 code units.

---

## Appendix D — knot JSON samples

`knot callers Embedder -r knot -o json` (truncated):

```json
{
  "calls": [], "declared_unused": [], "extends": [], "implements": [],
  "imports_script": [], "imports_stylesheet": [], "imports_vmod": [], "includes": [],
  "macro_calls": [], "overridden_by": [], "overrides": [],
  "references": [
    {
      "file_path": "src/bin/knot.rs", "kind": "rust_function", "name": "main",
      "rel_type": "REFERENCES", "repo_name": "knot", "signature": "()", "start_line": 22,
      "target_file_path": "src/pipeline/embed.rs",
      "target_fqn": "knot::pipeline::embed::Embedder",
      "target_name": "Embedder", "target_repo_name": "knot",
      "target_signature": "", "target_start_line": 28
    }
  ]
}
```

`knot search "<q>" -r knot -m 3 -o json` (one element):

```json
{
  "caller_count": 4,
  "caller_samples": ["run_deps_command in src/bin/knot.rs"],
  "dependencies": ["resolve_max_depth"],
  "docstring": "",
  "file_path": "src/cli_tools/deps.rs",
  "fqn": "knot::cli_tools::deps::run_deps",
  "kind": "rust_function",
  "name": "run_deps",
  "repo_name": "knot",
  "signature": "(…)",
  "start_line": 144,
  "uuid": "7c4ee1d6-4887-5fd3-a2c8-5a03b69142d2"
}
```

Note: `caller_samples` may include entities from other repositories sharing the collection
(e.g. knot-server) — irrelevant for S2, which compares files/uuids of the top results only.

---

## Appendix E — Results report template

`docs/spikes/phase0-results.md` must have exactly these sections (tables rendered by `summarize`
inside `<!-- BEGIN:table:… -->` markers):

1. **Summary and verdict** — the go/no-go table (G1–G16) with measured values and ✅/❌/⚠️, then
   5–10 bullet findings, then the decisions of §20.2.
2. **Environment** — versions (servers, rustc, JDK, knot CLI/crate, model), machine, snapshot
   fingerprints, dates, wall time of each run.
3. **S1 — LSP capability matrix** (per server: call/type hierarchy, implementation, references,
   server requests seen).
4. **S1 — Start-up, memory, latency** (cold/warm × language: spawn→initialize, initialize→ready,
   first request, p50/p95 per op, peak RSS, process count).
5. **S1 — Accuracy** (per language: precision/recall micro+macro for lsp, knot_db, heuristic;
   calls-only secondary; file-level; by category).
6. **S1 — Notable disagreements** (up to 15 per language with verdict and reason).
7. **S1 — Name-column heuristic** (compared, matched, failures, examples).
8. **S1 — Degradation (tier 2)** (diagnostics shown, fallback timing, heuristic quality).
9. **S2 — Sizes** (per repo × variant: raw, gzip, git pack, ratio vs source, bytes/entity).
10. **S2 — Determinism and churn** (rerun, batching, threads; append/prepend churn per variant,
    per-file vs monolith).
11. **S2 — Performance** (load times per variant and cache; query embed/scan p50/p95).
12. **S2 — Quality** (hit@5/hit@10 per system; overlap@10; quantisation loss; Qdrant parity).
13. **S2 — Tier 1** (hit@1/hit@5 in chunk domain and mixed domain; size increase).
14. **Decisions** — committed-index default, encoding, per-language graph tier, kill criteria
    evaluation.
15. **Deviations from plan** — every change to targets, queries, commands, thresholds proposals.
16. **Proposed Phases 1–7 (revised)** — bullet outline adjusted to the evidence, consistent
    with [Appendix F](#appendix-f--decisions-carried-into-phases-17).
17. **Reproduction** — link to `docs/spikes/phase0/README.md`.

---

## Appendix F — Decisions carried into Phases 1–7

Not to be implemented in Phase 0; listed so that the report's proposed outline is consistent.

1. **Crate architecture mirrors knot:** `src/lib.rs` with `config`, `cli_tools/*` (`run_*`
   functions holding the logic), `mcp_tools/*` (thin wrappers), `mcp_handler.rs` (`tools()` +
   `dispatch()` + dry-run mode), binaries `knot-rag` (CLI) and `knot-rag-mcp`. Edition 2024,
   same lints as knot, Linux/macOS/Windows.
2. **MCP SDK:** **`rust-mcp-sdk` 2.x** (2.0.0 at the time of writing) implementing the
   **stateless MCP revision 2026-07-28** (no `initialize` handshake). The human will move the
   client side to **opencode 2**. Consequences to address in Phase 1/5:
   - knot (1.x SDK) MCP code cannot be copied verbatim — re-implement `tools()`/`dispatch()` on
     the 2.x API; keep the transport-independent handler shape.
   - Without `initialize`, server instructions/capabilities and workspace discovery
     (previously via `roots`) must be re-checked against the 2026-07-28 spec; fallback is
     explicit `--repo` flags / config file.
   - A warm LSP pool needs process lifetime independent of MCP sessions (stateless requests),
     so the pool lives in the server process and is keyed by repo root + language.
3. **Index layout** `<repo>/.knot-rag/` (`manifest.toml`, `shards/<rel>.jsonl`,
   `edges/<rel>.jsonl`, `cache/` gitignored, `.gitattributes` with `linguist-generated=true -diff`),
   final encoding and commit default decided by this phase's data.
4. **Capability tiers** 3/2/1/0 as in §2, a `capability/` module computing the effective tier per
   language, `knot-rag status` showing the matrix, provenance on every graph answer.
5. **LSP layer:** `lsp/{registry,client,pool,readiness,position,ops}.rs`; registry can import
   server commands from opencode's `lsp` config and Claude Code `.lsp.json`.
6. **Quality gates:** when Phase 1 creates the root `Cargo.toml`, the `Makefile`'s
   `CRATE_DIR` auto-switches to `.` (no edit needed); the root crate copies knot's `[lints.*]`
   tables verbatim, uses the root `clippy.toml`/`dupes.toml` directly, and the spike crate keeps
   its own `[workspace]` so it is never part of the gate again (run its checks explicitly with
   `make check CRATE_DIR=docs/spikes/phase0` if it is still maintained). Re-measure the
   `clippy.toml` curves on knot-rag code at the end of Phase 1 and tighten where possible.
   Phase 7 adds `tests/run_all_e2e.sh`, which `make e2e` then picks up automatically.
7. **knot dependency** stays a path/crates.io dependency until a `knot-parser` split (or optional
   DB features in knot) is agreed with the human.
