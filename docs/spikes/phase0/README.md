# Phase 0 Spikes for knot-rag

Throw-away measurement crate for the two feasibility spikes of
[`docs/PLAN_PHASE0_SPIKES_0.md`](../PLAN_PHASE0_SPIKES_0.md) — **S1** (graph via LSP instead of
Neo4j) and **S2** (vectors in text files instead of Qdrant). Results and interpretation live in
[`docs/spikes/phase0-results.md`](../phase0-results.md).

## Prerequisites

- **Language servers:** `rust-analyzer` (rustup component), `typescript-language-server`
  (`npm i -g typescript-language-server typescript`), `jdtls` 1.61.0
  (`scripts/install_jdtls.sh`), OpenJDK 21, Maven 3.9.
- **knot:** CLI 1.11.x on `PATH`; the crate checkout used as the path dependency resolves from
  `Cargo.toml` (`../../../../knot`). Qdrant (`knot_entities`) and Neo4j running (parity + ground
  truth only).
- **Sandbox:** `scripts/setup_sandbox.sh` rsyncs `~/.knot/repos/{knot,knot-site,spring-ai}` into
  `/tmp/knot-rag-spike/work/` and records `FINGERPRINTS.txt`. Language servers only ever touch
  `/tmp/knot-rag-spike/` — never the real repositories.
- **python3** 3.11+ (`tomllib`), `curl`, `git`, `rsync`.
- **fastembed cache:** `~/.knot/repos/fastembed_cache` (via `KNOT_FASTEMBED_CACHE_DIR`).

## Reproduce everything

```bash
cd docs/spikes/phase0
bash scripts/run_all.sh            # ~3 h end to end (spring-ai dominates)
```

`scripts/run_all.sh` executes, in order (individual steps can be re-run alone):

| # | Step | Command | ~time |
|---|---|---|---|
| 0 | quality gates | `make check` (repo root) | 1 min |
| 1 | sandbox + ground truth | `setup_sandbox.sh`, `groundtruth.sh` | 2 min |
| 2 | validate targets | `lsp_probe validate-targets --targets …` | 2 min |
| 3 | S1 matrix | `lsp_probe run --targets … --run cold\|warm --out …` × 6 | 12 min |
| 4 | degradation | `lsp_probe nolsp … [--program /bin/false]` × 6 | 3 min |
| 5 | heuristic tier-2 | `lsp_probe heuristic …` × 3 | 1 min |
| 6 | gold metrics | `lsp_probe gold --lang …` × 3 | 2 min |
| 7 | S2 index + measure | `vec_probe index/measure …` × 3 | 35 min |
| 8 | determinism | `vec_probe determinism …` (4 index runs/repo) | 95 min |
| 9 | git pack + churn | `gitmeasure.sh` × 3 | 10 min |
| 10 | Qdrant parity | `qdrant_dump.sh` + `vec_probe parity` × 3 | 1 min |
| 11 | report | `summarize --results results --out ../phase0-results.md` | 1 s |

Step 6 reads the committed adjudication files (`results/adjudication/*.toml`); producing them is
manual by design — `lsp_probe gold` writes a `*.pending.toml` with code excerpts and the reviewer
fills in `verdict`/`reason`/`evidence` (T7.3).

Artefacts: `results/*.json` (committed) and `results/raw/*` (logs, gitignored). Shard corpora
under `/tmp/knot-rag-spike/s2/<repo>/`.

## Quality gates

`make check` at the **repository root** gates this crate (the root `Makefile` auto-selects it via
`CRATE_DIR` until Phase 1 creates a root crate):

- **clippy** picks up the root [`clippy.toml`](../../../clippy.toml) through clippy's parent
  directory lookup — no per-crate copy (T1.0.2).
- **cargo-dupes** reads `dupes.toml` only from the analysed directory, so
  [`dupes.toml`](./dupes.toml) is a **symlink** to the root file (zero ratchet); the Makefile
  refuses to run the gate without it (T1.0.3).
- Lint tables in [`Cargo.toml`](./Cargo.toml) are knot's `[lints.*]` byte-for-byte (D11).

Spike binaries are measurement tools and deliberately not installable; the measurement commands
are the table above (`run_all.sh` is the exact sequence used for the report).
