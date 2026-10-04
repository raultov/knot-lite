# knot-lite

Database-free code intelligence for **one repository**: file-based vectors plus a
language-server call graph, built on [knot](../knot)'s parsers. No Qdrant, no Neo4j,
no services to run — an index lives in a single `.knot-lite/` directory inside the
repository it describes.

> **Project status — Phase 1.** This release ships the `status` vertical slice: the
> per-repository capability matrix (CLI and MCP) and the four query tools
> (`search_hybrid_context`, `find_callers`, `explore_file`, `list_files`) registered
> with knot-identical input schemas. Until Phases 2–4 land the indexer and the query
> engines, those four tools answer with an honest "no index" diagnostic.

## Why this works

The Phase 0 feasibility spikes (`docs/spikes/phase0-results.md`) measured the two
backends this architecture rests on:

- **Language servers are the call graph.** LSP recall against an adjudicated gold set
  was 100 % (Rust) / 97 % (TypeScript) / 95 % (Java), with precision
  100 % / 100 % / 94.1 % — versus knot's Neo4j graph at 66 % / 95 % / 54 % recall.
- **File-based vectors are enough.** The `i8pv_b64` int8 product-quantised encoding
  kept top-10 overlap at 100 % versus f32 on all three benchmark repositories, at
  ~¼ of the storage.

## Capability tiers

Every language row in `status` reports one of these tiers (Phase 0 §2):

| Tier | Meaning | What knot-lite can answer |
|---|---|---|
| **3 — Full** | knot parser + language server found | entities + vectors; exact graph (LSP) |
| **2 — No LSP** | parser exists, language server missing | entities + vectors; **heuristic** graph |
| **1 — Files only** | no parser (`.sh`, `Makefile`, `Dockerfile`, …) | files and line ranges only |
| **0 — Excluded** | binaries, huge files, lock files | nothing (gitignored files are never counted) |

**Phase 1 caveat:** tier 3 means the language-server *binary was found*; the server is
started on demand in later phases.

## Install

```sh
make install          # builds release binaries and copies them to ~/.cargo/bin
```

Produces `knot-lite` (CLI) and `knot-lite-mcp` (stdio MCP server). Override the
destination with `make install DESTDIR=/some/dir`.

## CLI

```sh
knot-lite status                      # capability matrix as a table
knot-lite status -o json              # ... as JSON (schema knot-lite/status/v1)
knot-lite status -o markdown          # ... as GitHub-flavoured Markdown
knot-lite --repo-path ~/src/my-repo status
```

Query subcommands (they require an index and currently answer "no index"):

```sh
knot-lite search "user authentication" [-m 5] [-k rust_function] [-p src/]
knot-lite callers resolve_reference_intents [-m 25] [-k callable]
knot-lite explore src/services/user.ts [--include-markup]
knot-lite files [-p src/]
```

Exit codes:

| Code | Meaning |
|---|---|
| 0 | success (`status`) |
| 1 | any error (bad repository path, foreign `--repo`, I/O) |
| 2 | query unavailable (no index / unsupported / invalid / not implemented) |

## Configuration

Resolution precedence: CLI flags → process environment → `~/.config/knot-lite/.env`
(or `$KNOT_LITE_CONFIG_DIR/.env`) → defaults. The process environment is never
modified and the `.env` file is never written.

| Variable | Meaning |
|---|---|
| `KNOT_LITE_REPO_PATH` | repository root (default: current working directory) |
| `KNOT_LITE_REPO_NAME` | logical repository name (default: last path component) |
| `KNOT_LITE_CONFIG_DIR` | directory holding the `.env` file |
| `KNOT_LITE_LSP_RUST` | override for `rust-analyzer` (program name or path) |
| `KNOT_LITE_LSP_TS` | override for `typescript-language-server` |
| `KNOT_LITE_LSP_JAVA` | override for `jdtls` |
| `KNOT_LITE_DRY_RUN` | MCP only: answer protocol requests without a repository |
| `RUST_LOG` | log level (CLI defaults to `error`, MCP to `info`; logs go to stderr) |

## Language servers

`status` detects three servers and prints an install hint when one is missing:

| Language | Server | Install |
|---|---|---|
| Rust | `rust-analyzer` | `rustup component add rust-analyzer` |
| TypeScript / JavaScript | `typescript-language-server` | `npm install -g typescript-language-server typescript` |
| Java | `jdtls` 1.61+ | <https://download.eclipse.org/jdtls/milestones/> — put its `bin/` on `PATH` |

An override is used verbatim (no PATH fallback, so typos stay visible).

## MCP server

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

Environment variables can be passed with the `"environment"` key. Tools:
`status` (call it first), `search_hybrid_context`, `find_callers`, `explore_file`,
`list_files`. `repo_name` arguments are optional and accepted for compatibility with
knot (`'all'`, `'*'`, comma-separated lists, JSON arrays).

## Quality gates

```sh
make check                                  # fmt-check, clippy, every cargo test target, dupes
make test-lib                               # fast loop: library unit tests only (not a gate)
make check CRATE_DIR=docs/spikes/phase0     # the Phase 0 spike crate's own gate
```

`make check` runs everything `cargo test` runs (library and binary unit tests, the
`tests/mcp_stdio.rs` stdio integration test, doctests) plus the zero-duplication gate.
Spikes are excluded from the root duplication gate (see `dupes.toml`).

## Rename note

This project was previously called **knot-rag**. From Phase 1 on the product is
**knot-lite** (crate, binaries, `.knot-lite/` index, `KNOT_LITE_*`). Phase 0 artefacts
(`docs/PLAN_PHASE0_SPIKES_0.md`, `docs/spikes/`, the spike crate) keep the original
naming as a historical record.

## Plans

Phase plans live in `docs/PLAN_PHASE*.md`; conventions for agents are in
[`AGENTS.md`](AGENTS.md).
