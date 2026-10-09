# AGENTS.md — conventions for knot-lite

Rules for autonomous agents working in this repository. Phase plans (`docs/PLAN_*.md`, gitignored)
are local-only working documents provided by the human; they are never committed and may be
absent in a fresh clone. This file is the standing contract.

## Ground rules

1. **Write only inside** this repository (plus `~/.local/share/knot-lite/`, the
   persistent home of the measurement sandbox, outputs, hand-off notes and the E2E home
   `e2e/` with its work copies — never
   keep useful artefacts in a temp directory, and point `KNOT_LITE_LSP_CACHE_DIR`
   there for every run that starts a real language server). Only `index` writes into
   the analysed repository and only under `.knot-lite/`; the language servers' own
   scratch (`<cache>/<repo key>/`: rust-analyzer target dir, jdtls workspace, logs) is
   the one other place knot-lite makes them write to. A real language server must never
   leave a file in the repository: `autobuild` stays off for jdtls (it compiles into
   `target/` otherwise) and L5 proves it. (jdtls may create the two empty output
   directories of a Maven module; no file.)
   Never write into `../knot`, `~/.knot/repos/*` or any other repository. Read-only
   runs of `knot-lite status` on other checkouts are allowed. Never run `index` against `~/.knot/repos/*`.
2. **Rust:** edition 2024; **no `unsafe`** (denied in `Cargo.toml`); **no `#[allow]`**;
   `#[expect(..., reason = "...")]` only as a last resort and reported to the human.
   Fix clippy/dupes findings by refactoring — never silence them. Never loosen
   `clippy.toml`/`dupes.toml`.
3. All code, comments and docs in **English**.
4. **Cross-platform:** paths via `std::path`; no shelling out from Rust, except
   `src/lsp/process.rs`, which starts the language servers (the only module allowed to
   spawn them), and the test fixtures' `git init`; Windows executables via `PATHEXT`.
   Process signalling happens only in `src/lsp/process.rs` and `src/lsp/owner.rs`, and only
   for processes carrying the ownership mark of the server that started them (never by name).
5. **Unit tests inside library modules** (`#[cfg(test)] mod tests`). Binaries stay
   thin; their logic lives in the library. The only Rust test outside `src/` is
   `tests/mcp_stdio.rs` (the transport cannot be covered by lib tests). The shell E2E
   suites (`tests/run_*_e2e.sh`, `tests/e2e/lib.sh`) start real language servers and are
   not part of `make check`; run them with `make e2e`. Hermetic gate:
   tests in `src/` never download models, access network, start a real language server
   (an in-process scripted fake, `lsp/fake.rs`, speaks LSP instead; `config_for` turns
   servers off), or read `~/.knot` / the sandbox.
   Measurement tests are `#[ignore]`d in `src/measure.rs` and run explicitly with
   `KNOT_LITE_SANDBOX` and `KNOT_LITE_MEASURE_OUT` set (both required, no default).
6. After every task `make check` must be green before committing. Commits:
   `phase2(P2.x): <summary>`-style messages, one commit per task.
7. Use the knot MCP tools (`search_hybrid_context`, `find_callers`, `explore_file`)
   when reading `../knot` code.
8. If a plan is wrong about an API or a path, fix it conservatively and record the
   deviation in the plan's Appendix D.
9. Never commit `target/`, `.knot/`, `.knot-lite/`, `.idea/`, `docs/spikes/phase0/results/raw/`, or `docs/PLAN_*.md`.
   Stage files explicitly (no `git add -A`). Push, merge or tag only when the human explicitly asks;
   never push to or merge into `master` unprompted.

## Architecture — mirror knot

- Business logic lives in `cli_tools::run_*`; `mcp_tools/*` are thin wrappers;
  `mcp_handler` is transport-independent (`tools()` + `dispatch()`); binaries are
  minimal.
- The tool surface is knot's names with knot's exact input fields and constraints.
  The drift-guard tests in `mcp_handler.rs` and `mcp_tools/mod.rs` pin it.
- **One server = one repository.** `repo_name` arguments are validated by
  `models::repo_scope` and may only name the served repository (or `all`/`*`).
- **Never write outside `<repo>/.knot-lite/`.** Everything Phase 2 query tools do is read-only.
- Query tools fail as **tool errors** (`isError: true`), never as protocol errors, so
  the LLM can read the diagnostic and self-correct.

## Layout

```
src/
├── lib.rs            module list + crate docs
├── config.rs         Config, OutputFormat, CommonArgs, EmbedSettings, pure resolver
├── error.rs          IndexUnavailable, QueryUnavailable (typed diagnostics, exit code 2)
├── utils.rs          logging (stderr only)
├── measure.rs        #[cfg(test)] #[ignore] measurement gates M1–M8 (Phase 2) and L1–L12 (Phases 3–4)
├── models/           cli_args (clap), params (run_* contracts), repo_scope
├── index/            layout, manifest, select, inventory, record_io, encode, shard, edges, lsp_records, chunk, build, reuse, load
├── embed/            EmbedBatch trait, EmbedSettings, FakeEmbedder, KnotEmbedder
├── graph/            containers (ContainerTable), heuristic (resolve_edges), fanin (caller_fanin)
├── query/            session (QueryServices, IndexCache, EmbedderCache, LSP pool), filters, render, search, callers + sources (indexed / live / heuristic per target), explore, files
├── lsp/              pool/ (own thread + runtime: mod, factory, slot, start, worker; one lock per language), owner (ownership mark + sweeps), process (the only spawn), session, spec, target, normalize, request, precompute, probe, launch, readiness, preopen, position, uri, settings (+ fake, tests only)
├── capability/       language labels, which(), lsp_registry, scan, tier model
├── cli_tools/        run_status, run_index, run_* queries, run_cli
├── mcp_tools/        thin tool wrappers (parse_args, query_tool)
├── mcp_handler.rs    tools() + dispatch() + ServerHandler
└── bin/              knot-lite (CLI), knot-lite-mcp (stdio server)
tests/
├── mcp_stdio.rs      the stdio transport (Rust, part of `make check`)
├── run_all_e2e.sh    orchestrator of the E2E suites (`make e2e`)
├── run_{rust,typescript,javascript,java}_e2e.sh   one suite per language (Java also runs Gradle)
└── e2e/              lib.sh (helpers, sourced) and fixtures/ (copied to the E2E home, never used in place)
```

## Gates

```sh
make check                                  # fmt-check, clippy, cargo test (all targets), dupes
make test-lib                               # fast loop, not a gate
make check CRATE_DIR=docs/spikes/phase0     # the Phase 0 spike crate
make dupes-cleanup                          # stale duplication suppressions
make e2e                                    # the end-to-end suites (real servers; not in `make check`)
make check-all                              # make check + make e2e
```

Duplication must stay at **0** (see `dupes.toml`): extract a shared, parameterised
helper instead of duplicating code. `docs/spikes/` is excluded from the root gate.

## Adding a tool

1. Input struct in `mcp_tools/<tool>.rs` with `#[mcp_tool]` + `#[derive(JsonSchema,
   serde::Deserialize, Debug)]` and Appendix-A-style `#[json_schema(...)]` constraints.
2. `run_<tool>` in `cli_tools/` returning typed errors (extend `QueryUnavailable` if
   the diagnostic differs).
3. Parameters in `models/params.rs` (`from_cli` + `from_tool`, both unit-tested).
4. Register in `mcp_handler`: `tools()` + `dispatch()` + `EXPECTED_TOOLS` (tests).
5. CLI subcommand in `models/cli_args.rs` mirroring knot's flags, wired in
   `cli_tools::run_cli`.
6. Extend the schema tests in `mcp_tools/mod.rs` (one assertion per constraint) and
   the stdio test if the wire shape changed.
7. Update `README.md` and the plan's tool-surface appendix.

## Local Co-development

To build knot-lite against a local sibling checkout of `knot` (uncommitted):

```toml
# .cargo/config.toml (local, uncommitted): build against the sibling knot checkout
[patch.crates-io]
knot = { path = "../knot" }
```

## Plans

Phase plans (`docs/PLAN_*.md`, gitignored) are working documents provided by the human for execution.
