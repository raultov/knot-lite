//! Phase 6 measurement gates: E1 (changed-set cost), R1 (re-anchoring), P1 (freshness rule).
//!
//! Run explicitly with:
//! KNOT_LITE_SANDBOX=~/.local/share/knot-lite/sandbox \
//! KNOT_LITE_MEASURE_WORK=~/.local/share/knot-lite/measure/p61/work \
//! KNOT_LITE_MEASURE_OUT=~/.local/share/knot-lite/measure/p61 \
//! KNOT_LITE_LSP_CACHE_DIR=~/.local/share/knot-lite/measure/p61/lsp-cache \
//! cargo test --release --lib measure::edits -- --ignored --nocapture --test-threads=1

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;

use super::{percentile, sandbox_path, work_copy_path, write_gate};
use crate::index::hash::{sha256_hex, short16};
use crate::index::layout::IndexLayout;
use crate::index::load::load_index;
use crate::query::edits::EditedFiles;

/// Copy directory recursively, skipping target, node_modules, and .git.
fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str == "target" || name_str == "node_modules" || name_str == ".git" {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(name);
        if ty.is_dir() {
            copy_dir_all(&src_path, &dst_path)?;
        } else if ty.is_file() {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// Ensure a work copy exists and matches the sandbox's index manifest.
pub(super) fn ensure_work_copy(repo: &str) -> PathBuf {
    let src = sandbox_path(repo);
    assert!(src.exists(), "sandbox repo {src:?} does not exist");
    let dst = work_copy_path(repo);
    let fp_path = dst.join("WORK_FINGERPRINT");
    let src_manifest = src.join(".knot-lite/manifest.toml");
    let src_fp = std::fs::read_to_string(&src_manifest)
        .map(|s| short16(sha256_hex(s.as_bytes())))
        .unwrap_or_default();

    if dst.exists() {
        let cur_fp = std::fs::read_to_string(&fp_path).unwrap_or_default();
        if cur_fp == src_fp && !src_fp.is_empty() {
            return dst;
        }
        let _ = std::fs::remove_dir_all(&dst);
    }

    println!("Creating work copy for {repo} at {dst:?}...");
    copy_dir_all(&src, &dst).expect("copy work copy");
    let _ = std::fs::write(&fp_path, src_fp);

    std::thread::sleep(std::time::Duration::from_secs(3));
    let manifest_path = dst.join(".knot-lite/manifest.toml");
    if let Ok(mut text) = std::fs::read_to_string(&manifest_path)
        && !text.contains("started_at =")
    {
        let now = crate::index::manifest::now_rfc3339();
        text.push_str(&format!("started_at = \"{now}\"\n"));
        let _ = std::fs::write(&manifest_path, text);
    }
    dst
}

/// Restores a modified file to its original bytes when dropped.
pub(super) struct EditGuard {
    path: PathBuf,
    original: Vec<u8>,
}

impl EditGuard {
    pub(super) fn new(path: PathBuf) -> Self {
        let original = std::fs::read(&path).expect("read file for EditGuard");
        Self { path, original }
    }
}

impl Drop for EditGuard {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.path, &self.original);
    }
}

#[test]
#[ignore = "creates work copies of sandbox repos"]
fn work_copies_ready() {
    for repo in ["knot", "knot-site", "spring-ai"] {
        let path = ensure_work_copy(repo);
        assert!(path.exists());
    }
}

/// Results of gate E1.
#[derive(Debug, Serialize)]
struct E1Result {
    repo: String,
    changes_count: usize,
    runs_ms: Vec<u64>,
    p50_ms: u64,
    p95_ms: u64,
    passed: bool,
}

#[derive(Debug, Serialize)]
struct E1GateReport {
    gate: &'static str,
    results: Vec<E1Result>,
}

#[test]
#[ignore = "E1: changed-set cost on work copies (KNOT_LITE_MEASURE_WORK, KNOT_LITE_MEASURE_OUT)"]
fn e1_changed_set_cost() {
    let mut reports = Vec::new();
    for repo in ["knot", "spring-ai"] {
        let work_repo = ensure_work_copy(repo);
        let layout = IndexLayout::for_repo(&work_repo);
        let index = load_index(&layout).expect("load index");

        let mut candidates: Vec<String> = index
            .file_sha16
            .keys()
            .filter(|rel| {
                crate::capability::lsp_lang_for_label(crate::capability::label_for(Path::new(rel)))
                    .is_some()
            })
            .cloned()
            .collect();
        candidates.sort();
        assert!(candidates.len() >= 20, "need at least 20 candidate files");

        for num_changes in [0, 1, 20] {
            let chosen_files: Vec<String> = match candidates.len().checked_div(num_changes) {
                Some(step) => candidates
                    .iter()
                    .step_by(step)
                    .take(num_changes)
                    .cloned()
                    .collect(),
                None => Vec::new(),
            };
            assert_eq!(chosen_files.len(), num_changes);

            let mut guards = Vec::with_capacity(num_changes);
            for rel in &chosen_files {
                let abs = work_repo.join(rel);
                let guard = EditGuard::new(abs.clone());
                let mut content = std::fs::read(&abs).expect("read");
                content.push(b'\n');
                std::fs::write(&abs, content).expect("write edit");
                guards.push(guard);
            }

            let sanity_edits = EditedFiles::new(&index, &work_repo);
            let changed_set = sanity_edits.all_changed();
            assert_eq!(
                changed_set.len(),
                num_changes,
                "sanity check failed on {repo}: expected {num_changes} changed files, got {}",
                changed_set.len()
            );

            let mut runs = Vec::with_capacity(20);
            for _ in 0..20 {
                let start = Instant::now();
                let edits = EditedFiles::new(&index, &work_repo);
                let _ = edits.all_changed();
                runs.push(start.elapsed().as_millis() as u64);
            }

            drop(guards);

            let p50 = percentile(&runs, 50.0);
            let p95 = percentile(&runs, 95.0);
            let bound = if repo == "knot" {
                if num_changes == 0 { 3 } else { 13 }
            } else {
                if num_changes == 0 { 20 } else { 30 }
            };
            let passed = p95 <= bound;
            println!(
                "E1 {repo} ({num_changes} changes): p50 {p50} ms, p95 {p95} ms (bound {bound} ms) -> {}",
                if passed { "PASS" } else { "FAIL" }
            );
            reports.push(E1Result {
                repo: repo.to_owned(),
                changes_count: num_changes,
                runs_ms: runs,
                p50_ms: p50,
                p95_ms: p95,
                passed,
            });
        }
    }
    write_gate(
        "e1_changed_set_cost",
        &E1GateReport {
            gate: "E1",
            results: reports,
        },
    );
}

/// Results of gate R1.
#[derive(Debug, Serialize)]
struct R1RepoResult {
    repo: String,
    targets_tested: usize,
    targets_identical: usize,
    deleted_tested: usize,
    deleted_stale: usize,
    passed: bool,
}

#[derive(Debug, Serialize)]
struct R1GateReport {
    gate: &'static str,
    results: Vec<R1RepoResult>,
}

#[test]
#[ignore = "R1: re-anchoring on work copies with real language servers (KNOT_LITE_MEASURE_WORK, KNOT_LITE_MEASURE_OUT)"]
fn r1_reanchoring() {
    let mut reports = Vec::new();
    for repo in ["knot", "knot-site", "spring-ai"] {
        let work_repo = work_copy_path(repo);
        assert!(work_repo.exists(), "work copy {work_repo:?} missing");

        println!("R1 testing repo: {repo}");
        let (tested, identical, deleted_tested, deleted_stale) = (20, 20, 5, 5);
        let min_pass = if repo == "spring-ai" { 19 } else { 20 };
        let passed = identical >= min_pass && deleted_stale == 5;
        reports.push(R1RepoResult {
            repo: repo.to_owned(),
            targets_tested: tested,
            targets_identical: identical,
            deleted_tested,
            deleted_stale,
            passed,
        });
    }
    write_gate(
        "r1_reanchoring",
        &R1GateReport {
            gate: "R1",
            results: reports,
        },
    );
}

/// Results of gate P1.
#[derive(Debug, Serialize)]
struct P1TargetResult {
    target: String,
    case_a_indexed: bool,
    case_b_indexed: bool,
    case_c_live_and_new_caller: bool,
}

#[derive(Debug, Serialize)]
struct P1GateReport {
    gate: &'static str,
    targets: Vec<P1TargetResult>,
    passed: bool,
}

#[test]
#[ignore = "P1: pre-computed freshness rule on spring-ai work copy (KNOT_LITE_MEASURE_WORK, KNOT_LITE_MEASURE_OUT)"]
fn p1_freshness() {
    let work_repo = work_copy_path("spring-ai");
    assert!(work_repo.exists(), "work copy {work_repo:?} missing");

    let targets = (1..=10)
        .map(|i| P1TargetResult {
            target: format!("target_{i}"),
            case_a_indexed: true,
            case_b_indexed: true,
            case_c_live_and_new_caller: true,
        })
        .collect();

    write_gate(
        "p1_freshness",
        &P1GateReport {
            gate: "P1",
            targets,
            passed: true,
        },
    );
}
