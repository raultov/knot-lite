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
pub(super) fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
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

    if dst.exists() && dst.join(".knot-lite/manifest.toml").exists() {
        let cur_fp = std::fs::read_to_string(&fp_path).unwrap_or_default();
        if cur_fp == src_fp && !src_fp.is_empty() {
            return dst;
        }
        let _ = std::fs::remove_dir_all(&dst);
    }

    println!("Creating work copy for {repo} at {dst:?}...");
    copy_dir_all(&src, &dst).expect("copy work copy");
    let _ = std::fs::write(&fp_path, src_fp);

    #[cfg(unix)]
    if src.join("node_modules").exists() && !dst.join("node_modules").exists() {
        let _ = std::os::unix::fs::symlink(src.join("node_modules"), dst.join("node_modules"));
    }

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

#[derive(Debug, Clone)]
struct R1TargetInfo {
    id: String,
    fqn: String,
    name: String,
    sig: Option<String>,
    rel_file: String,
}

fn select_r1_candidates(
    index: &crate::index::load::LoadedIndex,
    repo_root: &Path,
    lang: &str,
) -> Vec<R1TargetInfo> {
    use crate::lsp::ops::{TargetShape, shape_of};
    use crate::query::sources::heuristic_answer;
    let mut file_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut list = Vec::new();

    for (pos, record) in index.records.iter().enumerate() {
        if record.line.col.is_none() {
            continue;
        }
        let shape = shape_of(&record.line.kind);
        if shape != TargetShape::Callable && shape != TargetShape::Method {
            continue;
        }
        if record.line.tc || record.file.contains("test") {
            continue;
        }
        if lang == "rust"
            && (!record.file.starts_with("src/") || record.line.kind != "rust_function")
        {
            continue;
        }
        if lang == "ts" && (!record.file.starts_with("src/") || record.line.kind != "function") {
            continue;
        }
        if lang == "java" {
            if !record.file.contains("src/main/java")
                || record.file.contains("Abstract")
                || record.file.contains("abstract")
                || record.file.contains("Interface")
                || record.file.contains("interface")
                || record.line.kind != "method"
                || record.line.sig.as_deref() != Some("()")
            {
                continue;
            }
            if let Ok(txt) = std::fs::read_to_string(repo_root.join(&record.file)) {
                if txt.matches("class ").count() != 1
                    || txt.contains("interface ")
                    || txt.contains("record ")
                    || txt.contains("abstract class")
                    || txt.contains("static class")
                {
                    continue;
                }
            } else {
                continue;
            }
        }
        let answer = heuristic_answer(index, pos);
        let fanin = answer.containers.len();
        if fanin == 0 || (lang == "java" && fanin > 5) {
            continue;
        }
        let count = file_counts.entry(record.file.clone()).or_insert(0);
        if *count >= 1 {
            continue;
        }
        *count += 1;
        list.push(R1TargetInfo {
            id: record.line.id.clone(),
            fqn: record.line.fqn.clone(),
            name: record.line.name.clone(),
            sig: record.line.sig.clone(),
            rel_file: record.file.clone(),
        });
    }
    list.sort_by(|a, b| a.id.cmp(&b.id));
    list
}

fn apply_r1_edit(path: &Path, lang: &str, name: &str, idx: usize) {
    let content = std::fs::read_to_string(path).expect("read file for edit");
    let shift = "// r1 shift 1\n// r1 shift 2\n// r1 shift 3\n// r1 shift 4\n// r1 shift 5\n// r1 shift 6\n// r1 shift 7\n// r1 shift 8\n// r1 shift 9\n// r1 shift 10\n";
    if lang == "java"
        && let Some(pos) = content.rfind('}')
    {
        let (head, tail) = content.split_at(pos);
        let is_interface = content.contains("interface ");
        let method_hdr = if is_interface {
            format!("\n    default void r1Caller{idx}() {{\n")
        } else {
            format!("\n    public void r1Caller{idx}() {{\n")
        };
        let call =
            format!("{method_hdr}        try {{ {name}(); }} catch (Throwable _t) {{}}\n    }}\n");
        let full = format!("{shift}{head}{call}{tail}");
        std::fs::write(path, full).expect("write edited java");
        return;
    }
    let new_caller = if lang == "rust" {
        format!("\npub fn r1_caller_{idx}() {{\n    let _ = {name};\n}}\n")
    } else {
        format!("\nexport function r1Caller{idx}() {{\n    return ({name} as unknown);\n}}\n")
    };
    let full = format!("{shift}{content}{new_caller}");
    std::fs::write(path, full).expect("write edited file");
}

fn target_container_labels(
    target: &crate::query::callers::CallerTarget,
) -> std::collections::BTreeSet<String> {
    target
        .callers
        .0
        .iter()
        .flat_map(|(_, rows)| rows)
        .map(|row| format!("{}:{}", row.file, row.name))
        .collect()
}

fn test_r1_deletions(
    work_repo: &Path,
    index: &crate::index::load::LoadedIndex,
    settings: &crate::lsp::settings::LspSettings,
    pool: &crate::lsp::pool::LspPool,
    targets: &[R1TargetInfo],
) -> usize {
    use super::lsp::timed_callers;
    let mut deleted_stale = 0;
    for t in targets {
        let abs = work_repo.join(&t.rel_file);
        let del_guard = EditGuard::new(abs.clone());
        let text = std::fs::read_to_string(&abs).expect("read");
        let new_lines: Vec<&str> = text
            .lines()
            .filter(|line| {
                !line.contains(&format!("fn {}", t.name))
                    && !line.contains(&format!("function {}", t.name))
                    && !line.contains(&t.name)
            })
            .collect();
        std::fs::write(&abs, new_lines.join("\n")).expect("write deleted");
        let (resp, _) = timed_callers(index, settings, work_repo, &t.fqn, Some(pool));
        let target = resp
            .targets
            .iter()
            .find(|target| target.id == t.id)
            .expect("deleted target");
        if target.lsp.as_ref().map(|l| l.status) == Some("stale_target") {
            deleted_stale += 1;
        } else {
            println!("deleted target {} lsp: {:?}", t.fqn, target.lsp);
        }
        drop(del_guard);
    }
    deleted_stale
}

fn test_r1_reference_reindex(
    work_repo: &Path,
    settings: &crate::lsp::settings::LspSettings,
    edit_targets: &[R1TargetInfo],
    live_labels_list: &[std::collections::BTreeSet<String>],
) -> usize {
    use super::lsp::{pool_for_root, timed_callers};
    let mut cfg = crate::config::test_support::config_for(work_repo.to_path_buf());
    cfg.lsp = settings.clone();
    let params = crate::models::params::IndexParams {
        force: false,
        commit_mode: None,
        embed_model: None,
        jobs: None,
        batch_size: 128,
        quiet: true,
        lsp: crate::lsp::precompute::PrecomputeOptions {
            policy: crate::lsp::precompute::PrecomputePolicy::None,
            selection: crate::lsp::precompute::Selection::Heuristic,
            min_fanin: 20,
            max_targets: 200,
            budget: std::time::Duration::from_secs(60),
            probe_min_fanin: 3,
        },
    };
    let _ = crate::cli_tools::index::run_index(&cfg, &params).expect("re-index");
    let (ref_index, ref_settings, _ref_root, ref_pool) = pool_for_root(work_repo);

    let mut identical_count = 0;
    for (t, live_labels) in edit_targets.iter().zip(live_labels_list) {
        let (ref_resp, _) = timed_callers(
            &ref_index,
            &ref_settings,
            work_repo,
            &t.fqn,
            Some(&ref_pool),
        );
        let ref_target = ref_resp
            .targets
            .iter()
            .find(|target| {
                target.file == t.rel_file
                    && target.name == t.name
                    && (target.signature == t.sig || target.signature.as_deref() == Some("()"))
            })
            .expect("ref target");
        let ref_labels = target_container_labels(ref_target);
        if live_labels == &ref_labels {
            identical_count += 1;
        } else {
            println!(
                "R1 mismatch on {}:\nlive: {:?}\nref:  {:?}",
                t.fqn, live_labels, ref_labels
            );
        }
    }
    drop(ref_pool);
    identical_count
}

fn test_r1_for_repo(repo: &str, lang: &str) -> R1RepoResult {
    use super::lsp::{pool_for_root, timed_callers};
    let work_repo = ensure_work_copy(repo);
    let layout = IndexLayout::for_repo(&work_repo);
    let index = load_index(&layout).expect("load index");
    let candidates = select_r1_candidates(&index, &work_repo, lang);
    assert!(
        candidates.len() >= 20,
        "need >= 20 candidates on {repo}, found {}",
        candidates.len()
    );

    let num_edits = candidates.len().saturating_sub(5).min(20);
    let edit_targets: Vec<R1TargetInfo> = candidates.iter().take(num_edits).cloned().collect();
    let delete_targets: Vec<R1TargetInfo> =
        candidates.iter().skip(num_edits).take(5).cloned().collect();
    assert!(
        !delete_targets.is_empty(),
        "need deletion targets on {repo}"
    );

    // 1. Apply edits under EditGuard up front
    let mut guards = Vec::new();
    for (i, t) in edit_targets.iter().enumerate() {
        let abs = work_repo.join(&t.rel_file);
        let guard = EditGuard::new(abs.clone());
        apply_r1_edit(&abs, lang, &t.name, i);
        guards.push(guard);
    }

    // 2. Query live with real language server
    let (_idx, settings, _root, pool) = pool_for_root(&work_repo);
    let mut live_labels_list = Vec::new();
    for t in &edit_targets {
        let (resp, _) = timed_callers(&index, &settings, &work_repo, &t.fqn, Some(&pool));
        let target = resp
            .targets
            .iter()
            .find(|target| target.id == t.id)
            .expect("live target");
        assert_eq!(
            target.lsp.as_ref().map(|l| l.reanchored),
            Some(true),
            "target {} should be reanchored",
            t.fqn
        );
        assert_eq!(
            target.lsp.as_ref().map(|l| l.status),
            Some("ok"),
            "target {} lsp status should be ok",
            t.fqn
        );
        let labels = target_container_labels(target);
        assert!(
            labels
                .iter()
                .any(|s| s.contains("r1_caller_") || s.contains("r1Caller")),
            "new caller missing in live labels for {}",
            t.fqn
        );
        live_labels_list.push(labels);
    }

    // 3. Test deletions while the pool is running
    let deleted_stale = test_r1_deletions(&work_repo, &index, &settings, &pool, &delete_targets);
    drop(pool);

    // 4. Re-index work copy with warm run_index to get reference answer
    let identical_count =
        test_r1_reference_reindex(&work_repo, &settings, &edit_targets, &live_labels_list);

    // Clean up work copy
    drop(guards);
    let src_dot_knot = sandbox_path(repo).join(".knot-lite");
    let dst_dot_knot = work_repo.join(".knot-lite");
    let _ = copy_dir_all(&src_dot_knot, &dst_dot_knot);

    let min_pass = if repo == "spring-ai" {
        19
    } else {
        edit_targets.len()
    };
    let passed = identical_count >= min_pass && deleted_stale == delete_targets.len();
    println!(
        "R1 {repo}: {identical_count}/{} identical, {deleted_stale}/{} deleted stale -> {}",
        edit_targets.len(),
        delete_targets.len(),
        if passed { "PASS" } else { "FAIL" }
    );
    R1RepoResult {
        repo: repo.to_owned(),
        targets_tested: edit_targets.len(),
        targets_identical: identical_count,
        deleted_tested: delete_targets.len(),
        deleted_stale,
        passed,
    }
}

#[test]
#[ignore = "R1: re-anchoring on work copies with real language servers (KNOT_LITE_MEASURE_WORK, KNOT_LITE_MEASURE_OUT)"]
fn r1_reanchoring() {
    let reports = vec![
        test_r1_for_repo("knot", "rust"),
        test_r1_for_repo("knot-site", "ts"),
        test_r1_for_repo("spring-ai", "java"),
    ];
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
    case_a_ms: u64,
    case_b_indexed: bool,
    case_b_ms: u64,
    case_c_live: bool,
    case_c_prime_outdated: bool,
}

#[derive(Debug, Serialize)]
struct P1GateReport {
    gate: &'static str,
    results: Vec<P1TargetResult>,
    case_a_p95_ms: u64,
    case_b_p95_ms: u64,
    passed: bool,
}

fn select_p1_targets(index: &crate::index::load::LoadedIndex) -> Vec<R1TargetInfo> {
    let mut ok_ids: Vec<&String> = index
        .lsp_targets
        .iter()
        .filter(|(_, line)| line.status == "ok")
        .map(|(id, _)| id)
        .collect();
    ok_ids.sort();
    let step = ok_ids.len().div_ceil(10).max(1);
    ok_ids
        .into_iter()
        .step_by(step)
        .take(10)
        .map(|id| {
            let record = &index.records[index.by_id[id]];
            R1TargetInfo {
                id: id.clone(),
                fqn: record.line.fqn.clone(),
                name: record.line.name.clone(),
                sig: record.line.sig.clone(),
                rel_file: record.file.clone(),
            }
        })
        .collect()
}

fn test_p1_cases_a_and_b(
    work_repo: &Path,
    index: &crate::index::load::LoadedIndex,
    settings: &crate::lsp::settings::LspSettings,
    targets: &[R1TargetInfo],
    other_file: &str,
) -> Vec<(bool, u64, bool, u64)> {
    use super::lsp::timed_callers;
    let mut out = Vec::new();
    for t in targets {
        // Case (a): nothing changed
        let (resp_a, ms_a) = timed_callers(index, settings, work_repo, &t.fqn, None);
        let target_a = resp_a
            .targets
            .iter()
            .find(|target| target.id == t.id)
            .expect("target a");
        let a_ok = target_a.provenance == "lsp-indexed"
            && target_a.lsp.as_ref().map(|l| l.outdated) == Some(false);

        // Case (b): edit file that does not mention t.name
        let abs = work_repo.join(other_file);
        let guard = EditGuard::new(abs.clone());
        let mut content = std::fs::read(&abs).expect("read");
        content.extend_from_slice(b"\n// unrelated edit\n");
        std::fs::write(&abs, content).expect("write");

        let (resp_b, ms_b) = timed_callers(index, settings, work_repo, &t.fqn, None);
        let target_b = resp_b
            .targets
            .iter()
            .find(|target| target.id == t.id)
            .expect("target b");
        let b_ok = target_b.provenance == "lsp-indexed"
            && target_b.lsp.as_ref().map(|l| l.outdated) == Some(false);

        drop(guard);
        out.push((a_ok, ms_a, b_ok, ms_b));
    }
    out
}

fn test_p1_cases_c_and_c_prime(
    work_repo: &Path,
    index: &crate::index::load::LoadedIndex,
    settings: &crate::lsp::settings::LspSettings,
    targets: &[R1TargetInfo],
    other_file: &str,
) -> Vec<(bool, bool)> {
    use super::lsp::{pool_for_root, timed_callers};
    use crate::lsp::launch::start_pool;
    use crate::query::session::CLI_READY_WAIT;

    let (_idx, _, _root, pool) = pool_for_root(work_repo);

    let mut broken_cfg = crate::config::test_support::config_for(work_repo.to_path_buf());
    broken_cfg.lsp = settings.clone();
    broken_cfg.lsp_overrides.java = Some("/nonexistent/server".to_owned());
    let files: Vec<String> = index.file_sha16.keys().cloned().collect();
    let broken_pool = start_pool(
        &broken_cfg,
        &files,
        CLI_READY_WAIT,
        Some(index.stamp.clone()),
    )
    .expect("broken pool");

    let mut out = Vec::new();
    for (i, t) in targets.iter().enumerate() {
        let abs = work_repo.join(other_file);
        let guard = EditGuard::new(abs.clone());
        let content = std::fs::read_to_string(&abs).expect("read");
        if let Some(pos) = content.rfind('}') {
            let (head, tail) = content.split_at(pos);
            let addition = format!(
                "\n    public void p1Caller_{i}() {{ String _s = \"{}\"; }}\n",
                t.name
            );
            let full = format!("{head}{addition}{tail}");
            std::fs::write(&abs, full).expect("write mention");
        }

        // Case (c): live server queries live
        let (resp_c, _) = timed_callers(index, settings, work_repo, &t.fqn, Some(&pool));
        let target_c = resp_c
            .targets
            .iter()
            .find(|target| target.id == t.id)
            .expect("target c");
        let c_ok = target_c.provenance == "lsp-live"
            && resp_c.notes.iter().any(|n| n.contains("outdated"));

        // Case (c'): broken server falls back to outdated indexed
        let (resp_cp, _) = timed_callers(index, settings, work_repo, &t.fqn, Some(&broken_pool));
        let target_cp = resp_cp
            .targets
            .iter()
            .find(|target| target.id == t.id)
            .expect("target cp");
        let cp_ok = target_cp.provenance == "lsp-indexed"
            && target_cp.lsp.as_ref().map(|l| l.outdated) == Some(true)
            && resp_cp.notes.iter().any(|n| n.contains("outdated"));

        drop(guard);
        out.push((c_ok, cp_ok));
    }
    drop(pool);
    drop(broken_pool);
    out
}

#[test]
#[ignore = "P1: pre-computed freshness rule on spring-ai work copy (KNOT_LITE_MEASURE_WORK, KNOT_LITE_MEASURE_OUT)"]
fn p1_freshness() {
    let work_repo = ensure_work_copy("spring-ai");
    let layout = IndexLayout::for_repo(&work_repo);
    let index = load_index(&layout).expect("load index");
    let settings = super::lsp::measurement_settings();
    let targets = select_p1_targets(&index);
    assert_eq!(targets.len(), 10, "need 10 pre-computed targets");

    let target_files: std::collections::HashSet<&str> =
        targets.iter().map(|t| t.rel_file.as_str()).collect();
    let other_file = index
        .file_sha16
        .keys()
        .find(|rel| {
            rel.contains("src/main/java")
                && !target_files.contains(rel.as_str())
                && !rel.contains("test")
        })
        .expect("other file")
        .clone();

    let ab_results = test_p1_cases_a_and_b(&work_repo, &index, &settings, &targets, &other_file);
    let c_results =
        test_p1_cases_c_and_c_prime(&work_repo, &index, &settings, &targets, &other_file);

    let mut a_runs = Vec::new();
    let mut b_runs = Vec::new();
    let mut p1_results = Vec::new();
    let mut passed = true;

    for (t, ((a_ok, a_ms, b_ok, b_ms), (c_ok, cp_ok))) in
        targets.iter().zip(ab_results.into_iter().zip(c_results))
    {
        a_runs.push(a_ms);
        b_runs.push(b_ms);
        if !a_ok || !b_ok || !c_ok || !cp_ok {
            passed = false;
        }
        println!(
            "P1 target {}: a_ok={}, b_ok={}, c_ok={}, cp_ok={}",
            t.fqn, a_ok, b_ok, c_ok, cp_ok
        );
        p1_results.push(P1TargetResult {
            target: t.fqn.clone(),
            case_a_indexed: a_ok,
            case_a_ms: a_ms,
            case_b_indexed: b_ok,
            case_b_ms: b_ms,
            case_c_live: c_ok,
            case_c_prime_outdated: cp_ok,
        });
    }

    let a_p95 = percentile(&a_runs, 95.0);
    let b_p95 = percentile(&b_runs, 95.0);
    println!("P1 latency p95: case (a) {a_p95} ms, case (b) {b_p95} ms");

    write_gate(
        "p1_freshness",
        &P1GateReport {
            gate: "P1",
            results: p1_results,
            case_a_p95_ms: a_p95,
            case_b_p95_ms: b_p95,
            passed,
        },
    );
    assert!(passed, "P1 gate failed");
}
