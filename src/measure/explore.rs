//! Phase 5 explore gates X1 (latency) and X2 (agreement with `find_callers`)
//! (PLAN_PHASE5_SEARCH_LSP_CI.md §11). The pool stays warm across calls, as in
//! an MCP session; the stdio transport is not part of the measurement.

use super::lsp::{pool_for_root, timed_callers};
use super::*;
use crate::lsp::ops::{TargetShape, shape_of};
use crate::query::callers::LspContext;
use crate::query::explore::{ExploreContext, ExploreResponse, explore_response};

fn explore(
    index: &LoadedIndex,
    settings: &crate::lsp::settings::LspSettings,
    root: &Path,
    pool: &crate::lsp::pool::LspPool,
    file: &str,
) -> (ExploreResponse, u64) {
    let started = Instant::now();
    let response = explore_response(&ExploreContext {
        index,
        repo: "measure",
        requested: file,
        include_markup: false,
        lsp: LspContext {
            settings,
            repo_root: root,
            pool: Some(pool),
        },
    });
    let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    (response, ms)
}

/// X1 on one repository: warm the server with the first file, then explore
/// every file three times; returns the report row.
fn x1(repo: &str, files: &[&str]) -> (u64, serde_json::Value) {
    let (index, settings, root, pool) = pool_for_root(&sandbox_path(repo));
    let warm = Instant::now();
    loop {
        let (response, _) = explore(&index, &settings, &root, &pool, files[0]);
        if response.lsp.status != "not_ready" || warm.elapsed() > Duration::from_secs(900) {
            break;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    let warm_ms = u64::try_from(warm.elapsed().as_millis()).unwrap_or(u64::MAX);
    let mut times = Vec::new();
    let mut statuses = BTreeMap::new();
    for _ in 0..3 {
        for file in files {
            let (response, ms) = explore(&index, &settings, &root, &pool, file);
            times.push(ms);
            *statuses.entry(response.lsp.status).or_insert(0usize) += 1;
        }
    }
    let p95 = percentile(&times, 95.0);
    println!("X1 {repo}: warm-up {warm_ms} ms, p95 {p95} ms, statuses {statuses:?}");
    (
        p95,
        serde_json::json!({"files": files, "warm_ms": warm_ms, "runs_ms": times, "p95_ms": p95, "statuses": statuses}),
    )
}

#[test]
#[ignore = "X1: explore latency with real rust-analyzer, typescript-language-server and jdtls on the sandbox (KNOT_LITE_LSP_CACHE_DIR; minutes)"]
fn x1_explore_latency() {
    let rust = x1(
        "knot",
        &[
            "src/cli_tools/find_callers.rs",
            "src/db/graph/query.rs",
            "src/pipeline/prepare.rs",
            "src/cli_tools/list_files.rs",
            "src/cli_tools/search_hybrid_context/rank.rs",
        ],
    );
    let ts = x1(
        "knot-site",
        &[
            "src/components/KnotServer.tsx",
            "src/components/TokenEfficiency.tsx",
            "functions/mcp.ts",
            "src/components/Installation.tsx",
            "src/components/Contact.tsx",
        ],
    );
    let java = x1(
        "spring-ai",
        &[
            "spring-ai-client-chat/src/main/java/org/springframework/ai/chat/client/DefaultChatClient.java",
            "spring-ai-model/src/main/java/org/springframework/ai/model/tool/DefaultToolCallingManager.java",
            "spring-ai-commons/src/main/java/org/springframework/ai/transformer/splitter/TokenTextSplitter.java",
        ],
    );
    write_gate(
        "x1_explore_latency",
        &serde_json::json!({"rust": rust.1, "ts": ts.1, "java_report_only": java.1}),
    );
    assert!(rust.0 <= 1500, "X1 rust: p95 {} ms > 1.5 s", rust.0);
    assert!(ts.0 <= 1500, "X1 ts: p95 {} ms > 1.5 s", ts.0);
}

/// X2 on one repository: for up to `want` type/value entities whose explore
/// count is live, the same count `find_callers` reports live.
fn x2(repo: &str, want: usize) -> (usize, usize, Vec<serde_json::Value>) {
    let (mut index, settings, root, pool) = pool_for_root(&sandbox_path(repo));
    index.lsp_targets.clear();
    let mut files: Vec<&String> = index
        .records
        .iter()
        .filter(|record| {
            record.line.col.is_some()
                && matches!(
                    shape_of(&record.line.kind),
                    TargetShape::TypeLike | TargetShape::ValueLike
                )
        })
        .map(|record| &record.file)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    files.truncate(want * 3);
    let (mut compared, mut agreed, mut rows) = (0, 0, Vec::new());
    for file in files {
        if compared == want {
            break;
        }
        let (response, _) = explore(&index, &settings, &root, &pool, file);
        let live = response
            .groups
            .iter()
            .flat_map(|group| group.entities.iter())
            .filter(|entity| entity.refs_provenance == Some("lsp-live"))
            .filter(|entity| {
                entity.id.as_ref().is_some_and(|id| {
                    let shape = shape_of(&index.records[index.by_id[id]].line.kind);
                    matches!(shape, TargetShape::TypeLike | TargetShape::ValueLike)
                })
            });
        for entity in live {
            if compared == want {
                break;
            }
            let id = entity.id.clone().expect("id");
            let fqn = index.records[index.by_id[&id]].line.fqn.clone();
            let (callers, _) = timed_callers(&index, &settings, &root, &fqn, Some(&pool));
            let Some(target) = callers.targets.iter().find(|t| t.id == id) else {
                continue;
            };
            if target.provenance != "lsp-live" {
                continue;
            }
            compared += 1;
            let same = entity.referenced_by == Some(target.caller_count);
            agreed += usize::from(same);
            rows.push(serde_json::json!({"fqn": fqn, "explore": entity.referenced_by, "callers": target.caller_count, "same": same}));
        }
    }
    println!("X2 {repo}: {agreed}/{compared} agree");
    (compared, agreed, rows)
}

#[test]
#[ignore = "X2: explore vs find_callers with real rust-analyzer and typescript-language-server on the sandbox (KNOT_LITE_LSP_CACHE_DIR; minutes)"]
fn x2_explore_agreement() {
    let rust = x2("knot", 20);
    let ts = x2("knot-site", 20);
    write_gate(
        "x2_explore_agreement",
        &serde_json::json!({
            "rust": {"compared": rust.0, "agreed": rust.1, "rows": rust.2},
            "ts": {"compared": ts.0, "agreed": ts.1, "rows": ts.2},
        }),
    );
    for (name, (compared, agreed, _)) in [("rust", rust), ("ts", ts)] {
        assert_eq!(
            compared, 20,
            "X2 {name}: only {compared} comparable entities"
        );
        assert_eq!(agreed, compared, "X2 {name}: {agreed}/{compared} agree");
    }
}
