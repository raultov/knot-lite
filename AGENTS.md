# AGENTS.md — conventions for knot-lite

Rules for autonomous agents working in this repository. The phase plans
(`docs/PLAN_PHASE*.md`) carry the full context; this file is the standing contract.

## Ground rules

1. **Write only inside** this repository (plus `/tmp/knot-lite-*` scratch dirs).
   Never write into `../knot`, `~/.knot/repos/*` or any other repository. Read-only
   runs of `knot-lite status` on other checkouts are allowed.
2. **Rust:** edition 2024; **no `unsafe`** (denied in `Cargo.toml`); **no `#[allow]`**;
   `#[expect(..., reason = "...")]` only as a last resort and reported to the human.
   Fix clippy/dupes findings by refactoring — never silence them. Never loosen
   `clippy.toml`/`dupes.toml`.
3. All code, comments and docs in **English**.
4. **Cross-platform:** paths via `std::path`; no shelling out from Rust (the test
   fixtures' `git init` is the only exception); Windows executables via `PATHEXT`.
5. **Unit tests inside library modules** (`#[cfg(test)] mod tests`). Binaries stay
   thin; their logic lives in the library. The only test outside `src/` is
   `tests/mcp_stdio.rs` (the transport cannot be covered by lib tests).
6. After every task `make check` must be green before committing. Commits:
   `phase1(P1.x): <summary>`-style messages, one commit per task.
7. Use the knot MCP tools (`search_hybrid_context`, `find_callers`, `explore_file`)
   when reading `../knot` code.
8. If a plan is wrong about an API or a path, fix it conservatively and record the
   deviation in the plan's Appendix D.
9. Never commit `target/`, `.knot/`, `.idea/`, `docs/spikes/phase0/results/raw/`.
   Stage files explicitly (no `git add -A`). Never push, tag or merge into `master`.

## Architecture — mirror knot

- Business logic lives in `cli_tools::run_*`; `mcp_tools/*` are thin wrappers;
  `mcp_handler` is transport-independent (`tools()` + `dispatch()`); binaries are
  minimal.
- The tool surface is knot's names with knot's exact input fields and constraints
  (`docs/PLAN_PHASE1_SKELETON.md` Appendix A). Drift-guard tests in `mcp_handler.rs`
  and `mcp_tools/mod.rs` pin it.
- **One server = one repository.** `repo_name` arguments are validated by
  `models::repo_scope` and may only name the served repository (or `all`/`*`).
- **Never write into the analysed repository.** Everything Phase 1 does is read-only;
  later phases write only under `<repo>/.knot-lite/`.
- Query tools fail as **tool errors** (`isError: true`), never as protocol errors, so
  the LLM can read the diagnostic and self-correct.

## Layout

```
src/
├── lib.rs            module list + crate docs
├── config.rs         Config, OutputFormat, CommonArgs, pure resolver
├── error.rs          QueryUnavailable (typed diagnostics, exit code 2)
├── utils.rs          logging (stderr only)
├── models/           cli_args (clap), params (run_* contracts), repo_scope
├── index/            layout constants, manifest reading (read-only in Phase 1)
├── capability/       language labels, which(), lsp_registry, scan, tier model
├── cli_tools/        run_status / run_* queries / run_cli
├── mcp_tools/        thin tool wrappers (parse_args, query_tool)
├── mcp_handler.rs    tools() + dispatch() + ServerHandler
└── bin/              knot-lite (CLI), knot-lite-mcp (stdio server)
```

## Gates

```sh
make check                                  # fmt-check, clippy, cargo test (all targets), dupes
make test-lib                               # fast loop, not a gate
make check CRATE_DIR=docs/spikes/phase0     # the Phase 0 spike crate
make dupes-cleanup                          # stale duplication suppressions
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

## Plans

`docs/PLAN_PHASE0_SPIKES_0.md`, `docs/PLAN_PHASE1_SKELETON.md` (Appendix D holds the
execution notes and deviations), and later `docs/PLAN_PHASE*.md`.
