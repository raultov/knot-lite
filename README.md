# knot-lite

Database-free code intelligence for **one repository**: file-based vectors plus a
language-server call graph, built on [knot](../knot)'s parsers. No Qdrant, no Neo4j,
no services to run — an index lives in a single `.knot-lite/` directory inside the
repository it describes.

> **Project status — Phase 3.** All five tools (`status`, `search_hybrid_context`, `find_callers`,
> `explore_file`, `list_files`) answer from the index. `find_callers` now asks the **language
> server** (rust-analyzer, typescript-language-server, jdtls) per target — live, or from edges
> pre-computed by `knot-lite index` for widely used targets — and falls back to knot's heuristic
> edges per target when a server is unavailable. Vectors use `i8pv_b64` int8 product-quantisation
> with text-hash reuse.

## Quick start

```sh
knot-lite index                               # build .knot-lite/ in the current directory
knot-lite status                              # check capability matrix, index state and freshness
knot-lite search "user authentication"        # vector search
knot-lite callers parse_files                 # reverse references (language server, heuristic fallback)
knot-lite callers parse_files --no-lsp        # same, without asking language servers
knot-lite status --probe-lsp                  # start each language server once and report readiness
knot-lite explore src/pipeline/parser/mod.rs  # file anatomy and entity list
knot-lite files -p src/                       # list indexed files
```

## Tools & Capabilities (0.3.0)

| Tool | Capability | Provenance |
|---|---|---|
| `status` | Capability matrix (tiers 3/2/1/0 per language) + index details, counts, model, freshness; language-server state (running, version, memory, pre-computed or not) | — |
| `search_hybrid_context` | Cosine top-k vector search over entities and tier-1 line-window chunks | `vector` |
| `find_callers` | 3-tier target resolution, container-level callers grouped by relationship; per-target source | `lsp-live` / `lsp-indexed` / `heuristic` |
| `explore_file` | File entities grouped by family, with incoming-reference counts and markup summaries | — |
| `list_files` | Indexed files with entity and chunk counts, prefix filter | — |

## Indexing (`knot-lite index`)

`knot-lite index` builds `<repo>/.knot-lite/`:
- `manifest.toml`: index metadata, counts, model, timings, commit mode.
- `inventory.jsonl`: file path, size, and SHA-256 hash stamp for freshness detection.
- `shards/<rel>.jsonl`: int8 quantised vector records for parsed code entities and tier-1 line-window chunks.
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
  and the note says so — retry for language-server answers.
- **When they stop.** The CLI stops them when it exits. The MCP server stops a server unused for
  `KNOT_LITE_LSP_IDLE_SECS` (default 900) and restarts it on demand; a server that dies is restarted after a
  back-off (5 s, then 30 s, 120 s) and disabled after three failures.
- **Where they write.** Only under `<repo>/.knot-lite/` (the index) and the per-repository scratch directory
  `$KNOT_LITE_LSP_CACHE_DIR/<repo key>/` (rust-analyzer's target dir, jdtls's workspace, the last 5 stderr logs
  per language). Measured: the repository is left byte-identical (see Security for the one thing this relies on).
- **Budget per call.** At most `KNOT_LITE_LSP_MAX_LIVE_TARGETS` (default 10) live targets and
  `KNOT_LITE_LSP_DEADLINE_SECS` (default 30 s) of live time per `find_callers`; the rest fall back, with a note.
- **Memory** (resident, measured): rust-analyzer ≈ 3.1 GiB on a 4.8 k-entity repository, jdtls ≈ 1.4 GiB on
  `spring-ai` (it may grow up to its `-Xmx8g`), typescript-language-server ≈ 0.6 GiB on a 1.5 k-entity repository.
  `status` shows each running server's pid, readiness time and memory.

## Security

Starting a language server on a repository **runs that repository's build code**, as opening it in an editor
does: rust-analyzer runs build scripts and proc-macros (`cargo check` into the scratch directory), jdtls
imports Maven/Gradle models, and tsserver loads the project's TypeScript. Only run knot-lite on repositories
you would open in an editor. Set `KNOT_LITE_LSP_BUILD_EXEC=off` to disable rust-analyzer build scripts and
proc-macros and jdtls's Gradle import (accuracy drops where code is generated), or `KNOT_LITE_LSP=off` to never
start a server (pre-computed results are still read). jdtls is started with its auto-build off so that it does
not compile into `target/` of your modules, and with Eclipse metadata files disabled; on a Maven repository
(`spring-ai`) the working tree was measured byte-identical before and after. **Gradle projects are the
exception while build execution is on**: jdtls runs Gradle on them, which writes a `.gradle/` project cache
into the project, may download a Gradle distribution into `~/.gradle` and starts a Gradle daemon that outlives
knot-lite. Use `KNOT_LITE_LSP_BUILD_EXEC=off` on repositories with Gradle projects you do not want touched. The
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

## Limitations

- **Pure Vector Search:** `search_hybrid_context` ranks by cosine similarity over int8 vectors. No BM25 keyword channel, no kind-aware re-ranking, and no graph boost.
- **Heuristic fallback:** for languages without a server, and whenever a server is unavailable, still starting or the target's file changed since the index, `find_callers` uses knot's heuristic edges (~93 % precision, ~86 % recall on Rust; indirect or dynamic calls may be missed). A changed file falls back until `knot-lite index` runs again; unsaved edits are not sent to servers.
- **Language servers are per call:** only the first `KNOT_LITE_LSP_MAX_LIVE_TARGETS` targets of a call are asked; `explore_file` and `search_hybrid_context` do not use servers. Pre-computed answers do not report self-references.
- **One target at a time per language:** queries of the same language are answered one after another; different languages run in parallel, and `status` never waits for a query.
- **Prefix Filters Only:** Path filters accept repo-relative directory prefixes (e.g. `src/api`). Glob patterns are rejected with a pinned diagnostic.

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
| `KNOT_LITE_LSP` | `on` (default) / `off`: master switch for starting language servers (pre-computed data is still read when `off`) |
| `KNOT_LITE_LSP_BUILD_EXEC` | `on` (default) / `off`: no rust-analyzer build scripts or proc-macros, no jdtls Gradle import |
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
```

The measurement gates (M1/M4/M5/M6 and the Phase 3 L1/L2/L3/L7, `#[ignore]`d in `src/measure.rs`) run
explicitly and require persistent directories — there is no temp-dir default, so results are never lost. The
L gates start **real language servers** on the sandbox copies and also need `KNOT_LITE_LSP_CACHE_DIR`:

```sh
KNOT_LITE_SANDBOX=~/.local/share/knot-lite/sandbox \
KNOT_LITE_MEASURE_OUT=~/.local/share/knot-lite/measure \
cargo test --release --lib measure:: -- --ignored --nocapture --test-threads=1
```

## Plans & Conventions

Spike results live in [`docs/spikes/phase0-results.md`](docs/spikes/phase0-results.md);
conventions for agents are in [`AGENTS.md`](AGENTS.md).
