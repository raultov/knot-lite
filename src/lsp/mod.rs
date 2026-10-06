//! The language-server layer (Phase 3): `find_callers` answers for Rust,
//! TypeScript/JavaScript and Java come from `rust-analyzer`,
//! `typescript-language-server` and `jdtls` whenever the binary is found, and
//! fall back to the heuristic edges of the index per target (P3-D1).
//!
//! **Process model (P3-D8).** One pool per process owns its own Tokio
//! runtime on a dedicated OS thread and exposes a *synchronous* API, so it can
//! be called from the CLI, from an MCP `spawn_blocking` thread or from index
//! building without ever calling `block_on` inside a runtime. Servers start
//! lazily, are shut down when idle, and are restarted a bounded number of times.
//!
//! **Containment (P3-D12).** knot-lite writes only under `<repo>/.knot-lite/`
//! and under the per-repository scratch directory of [`settings::scratch_dir`]
//! (rust-analyzer target dir, jdtls workspace, server logs). The spawn of the
//! server processes is confined to one module (P3-D6).
//!
//! **Hermetic tests (P3-D13).** Nothing reachable by `make check` starts a real
//! language server: an in-process scripted fake speaks LSP over a duplex pipe.

pub mod settings;
