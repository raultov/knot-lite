# knot-lite

Database-free code intelligence for **one repository**: file-based vectors plus a
language-server call graph, built on [knot](../knot)'s parsers. No Qdrant, no Neo4j,
no services to run — an index lives in a single `.knot-lite/` directory inside the
repository it describes.

> **Project status — Phase 2.** This release ships `knot-lite index` and all five tools answering
> from the index (`status`, `search_hybrid_context`, `find_callers`, `explore_file`, `list_files`).
> Vectors use `i8pv_b64` int8 product-quantisation with text-hash reuse; reverse dependencies are
> pre-computed at index time by knot's heuristic resolver.

## Quick start

```sh
knot-lite index                               # build .knot-lite/ in the current directory
knot-lite status                              # check capability matrix, index state and freshness
knot-lite search "user authentication"        # vector search
knot-lite callers parse_files                 # reverse references (heuristic call graph)
knot-lite explore src/pipeline/parser/mod.rs  # file anatomy and entity list
knot-lite files -p src/                       # list indexed files
```

## Tools & Capabilities (0.2.0)

| Tool | Capability | Provenance |
|---|---|---|
| `status` | Capability matrix (tiers 3/2/1/0 per language) + index details, counts, model, freshness | — |
| `search_hybrid_context` | Cosine top-k vector search over entities and tier-1 line-window chunks | `vector` |
| `find_callers` | 3-tier target resolution, container-level callers grouped by relationship | `heuristic` |
| `explore_file` | File entities grouped by family, with incoming-reference counts and markup summaries | — |
| `list_files` | Indexed files with entity and chunk counts, prefix filter | — |

## Indexing (`knot-lite index`)

`knot-lite index` builds `<repo>/.knot-lite/`:
- `manifest.toml`: index metadata, counts, model, timings, commit mode.
- `inventory.jsonl`: file path, size, and SHA-256 hash stamp for freshness detection.
- `shards/<rel>.jsonl`: int8 quantised vector records for parsed code entities and tier-1 line-window chunks.
- `edges/<rel>.jsonl`: pre-computed reference relationship edges, keyed by caller file.
- `.gitignore` (default) or `.gitattributes` (`--commit`).

Index flags:
- `--force`: ignore reusable vectors and re-embed everything.
- `--commit`: track `.knot-lite/` in git (writes `.gitattributes`). Default is `--gitignore`.
- `--embed-model <model>`: embedding model choice (`AllMiniLML6V2` default, `BGEBaseENV15` opt-in).
- `-j, --jobs <N>`: parallel parser threads.
- `--batch-size <N>`: passages per embedding call (default: 128).
- `-q, --quiet`: suppress progress output on stderr.

## Capability tiers

Every language row in `status` reports one of these tiers (Phase 0 §2):

| Tier | Meaning | What knot-lite can answer |
|---|---|---|
| **3 — Full** | knot parser + language server found | entities + vectors; exact graph (LSP on-demand in later phases) |
| **2 — No LSP** | parser exists, language server missing | entities + vectors; **heuristic** graph |
| **1 — Files only** | no parser (`.sh`, `Makefile`, `Dockerfile`, …) | line-window chunks and file-level vectors |
| **0 — Excluded** | binaries, huge files (> 256 KiB), lock files | nothing (gitignored files are never counted) |

## Performance & Baselines (Phase 2 Measurements)

- **Cold Index Time:** ~150 s for `knot` (~5.8k entities), ~24 s for `knot-site` (~1.5k entities), ~12 min for `spring-ai` (~33.5k entities).
- **Warm Re-index Time:** ~0.6 s for `knot`, ~0.2 s for `knot-site` (0 vectors re-embedded).
- **Single Function Append:** ~0.6 s for `knot` (1 vector re-embedded, wall time << 10% of cold run).
- **Index Load Median Latency:** 16 ms for `knot`, 3 ms for `knot-site`, 163 ms for `spring-ai`.
- **Retrieval Accuracy (Entity Domain hit@5):** `knot` 75.0 %, `knot-site` 80.0 %, `spring-ai` 83.3 %.

## Limitations

- **Pure Vector Search:** `search_hybrid_context` ranks by cosine similarity over int8 vectors. No BM25 keyword channel, no kind-aware re-ranking, and no graph boost.
- **Heuristic Call Graph:** `find_callers` uses pre-computed edges from knot's heuristic resolver. Expect ~93 % precision and ~86 % recall on Rust; indirect or dynamic calls may be missed.
- **Prefix Filters Only:** Path filters accept repo-relative directory prefixes (e.g. `src/api`). Glob patterns are rejected in 0.2.0.

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
| `KNOT_LITE_FASTEMBED_CACHE_DIR` | fastembed model cache directory (fallback: `KNOT_FASTEMBED_CACHE_DIR` or `~/.knot/repos/fastembed_cache`) |
| `KNOT_LITE_CONFIG_DIR` | directory holding the `.env` file |
| `KNOT_LITE_LSP_RUST` | override for `rust-analyzer` (program name or path) |
| `KNOT_LITE_LSP_TS` | override for `typescript-language-server` |
| `KNOT_LITE_LSP_JAVA` | override for `jdtls` |
| `KNOT_LITE_DRY_RUN` | MCP only: answer protocol requests without a repository |
| `RUST_LOG` | log level (CLI defaults to `error`, MCP to `info`; logs go to stderr) |

## Troubleshooting

- **Incomplete index:** If an index run is interrupted, `status` reports `index: incomplete`. Run `knot-lite index` to complete it.
- **Model download:** First index run downloads ONNX model weights (~23 MB for `AllMiniLML6V2`). Reuse an existing cache by setting `KNOT_FASTEMBED_CACHE_DIR=~/.knot/repos/fastembed_cache`.
- **Corrupt index / model switch:** Run `knot-lite index --force` to rebuild shards from scratch.

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

## Plans & Conventions

Spike results live in [`docs/spikes/phase0-results.md`](docs/spikes/phase0-results.md);
conventions for agents are in [`AGENTS.md`](AGENTS.md).
