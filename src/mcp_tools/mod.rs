//! Thin MCP tool wrappers over the `cli_tools::run_*` functions.
//!
//! Every tool file declares the `#[mcp_tool]` input struct (Appendix A) and a
//! one-line `handle` that delegates here; the shared machinery keeps each tool
//! file tiny and the diagnostics identical.

use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::config::{Config, OutputFormat};
use crate::mcp_handler::KnotLiteMcpHandler;
use crate::models::repo_scope::check_repo_scope;
use crate::query::session::QueryServices;
use rust_mcp_sdk::schema::{CallToolError, CallToolRequestParams, CallToolResult, TextContent};

pub mod explore_file;
pub mod find_callers;
pub mod list_files;
pub mod search_hybrid_context;
pub mod status;

/// Specification for executing a query tool inside MCP.
pub(crate) struct QueryToolSpec<T, P> {
    pub to_params: fn(T) -> P,
    pub run: fn(&Config, &QueryServices, &P) -> anyhow::Result<Value>,
    pub format: fn(&Value, OutputFormat) -> String,
}

/// Deserialize tool arguments after validating and removing `repo_name`
/// (knot accepts arrays for it; the typed struct only models the string form).
pub(crate) fn parse_args<T: DeserializeOwned>(
    tool: &str,
    params: &CallToolRequestParams,
    served_repo: &str,
) -> Result<T, CallToolError> {
    let mut args = params.arguments.clone().unwrap_or_default();
    if let Err(e) = check_repo_scope(args.get("repo_name"), served_repo) {
        return Err(CallToolError::from_message(format!("{tool}: {e}")));
    }
    args.remove("repo_name");
    serde_json::from_value(Value::Object(args))
        .map_err(|e| CallToolError::from_message(format!("invalid arguments for {tool}: {e}")))
}

/// Common body of the four query tools.
///
/// The runner is synchronous and may wait for a language server, so it runs on
/// a blocking thread: the async runtime keeps serving other requests (P3-D16).
pub(crate) async fn query_tool<T, P>(
    tool: &str,
    params: CallToolRequestParams,
    handler: &KnotLiteMcpHandler,
    spec: QueryToolSpec<T, P>,
) -> Result<CallToolResult, CallToolError>
where
    T: DeserializeOwned,
    P: Send + 'static,
{
    let cfg = handler.config()?.clone();
    let args = parse_args::<T>(tool, &params, &cfg.repo_name)?;
    let tool_params = (spec.to_params)(args);
    let services = Arc::clone(&handler.services);
    let run = spec.run;
    let joined = tokio::task::spawn_blocking(move || run(&cfg, &services, &tool_params)).await;
    let outcome = joined
        .map_err(|error| CallToolError::from_message(format!("{tool}: internal error: {error}")))?;
    match outcome {
        Ok(json) => {
            let markdown = (spec.format)(&json, OutputFormat::Markdown);
            let mut result =
                CallToolResult::text_content(vec![TextContent::new(markdown, None, None)]);
            if let Value::Object(obj) = json {
                result = result.with_structured_content(obj);
            }
            Ok(result)
        }
        Err(e) => Err(CallToolError::from_message(format!("{e:#}"))),
    }
}

/// Generates the delegating `handle` method of one query tool.
///
/// The logic lives once, in [`query_tool` philosophy]; a tool only wires its
/// input type, its `cli_tools::run_*` runner, and its formatter.
macro_rules! impl_query_tool_handle {
    ($tool:ty, $name:literal, $params:ty, $run:path, $format:path) => {
        impl $tool {
            /// Handle a tool call by delegating to the shared query machinery.
            pub async fn handle(
                params: rust_mcp_sdk::schema::CallToolRequestParams,
                handler: &crate::mcp_handler::KnotLiteMcpHandler,
            ) -> Result<rust_mcp_sdk::schema::CallToolResult, rust_mcp_sdk::schema::CallToolError>
            {
                $crate::mcp_tools::query_tool::<Self, _>(
                    $name,
                    params,
                    handler,
                    $crate::mcp_tools::QueryToolSpec {
                        to_params: <$params>::from_tool,
                        run: $run,
                        format: $format,
                    },
                )
                .await
            }
        }
    };
}
pub(crate) use impl_query_tool_handle;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_tools::find_callers::FindCallersTool;
    use rust_mcp_sdk::schema::{ClientCapabilities, RequestMetaObject, Tool};
    use serde_json::json;

    fn call_params(name: &str, args: serde_json::Map<String, Value>) -> CallToolRequestParams {
        CallToolRequestParams {
            name: name.to_string(),
            arguments: Some(args),
            input_responses: None,
            request_state: None,
            meta: RequestMetaObject::new("2026-07-28", ClientCapabilities::default()),
        }
    }

    fn all_tools() -> Vec<Tool> {
        crate::mcp_handler::KnotLiteMcpHandler::tools()
    }

    fn tool_named(name: &str) -> Tool {
        all_tools()
            .into_iter()
            .find(|tool| tool.name == name)
            .unwrap_or_else(|| panic!("tool {name} not registered"))
    }

    fn schema_map(tool: &Tool) -> &serde_json::Map<String, Value> {
        tool.input_schema
            .extra
            .as_ref()
            .unwrap_or_else(|| panic!("no schema properties on {}", tool.name))
    }

    fn property<'a>(tool: &'a Tool, name: &str) -> &'a Value {
        schema_map(tool)
            .get("properties")
            .and_then(Value::as_object)
            .and_then(|props| props.get(name))
            .unwrap_or_else(|| panic!("property {name} missing on {}", tool.name))
    }

    #[test]
    fn all_tools_have_valid_names() {
        for tool in all_tools() {
            assert!(!tool.name.is_empty());
            assert!(
                tool.name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_'),
                "unexpected tool name {}",
                tool.name
            );
        }
    }

    #[test]
    fn all_tools_have_descriptions() {
        for tool in all_tools() {
            assert!(tool.description.as_deref().is_some_and(|d| !d.is_empty()));
        }
    }

    #[test]
    fn descriptions_contain_phase2_keywords() {
        assert!(
            tool_named("search_hybrid_context")
                .description
                .unwrap()
                .contains("no keyword channel")
        );
        assert!(
            tool_named("find_callers")
                .description
                .unwrap()
                .contains("heuristic")
        );
        for name in [
            "search_hybrid_context",
            "find_callers",
            "explore_file",
            "list_files",
        ] {
            assert!(
                tool_named(name)
                    .description
                    .unwrap()
                    .contains("knot-lite index")
            );
        }
    }

    #[test]
    fn descriptions_contain_phase3_keywords() {
        let callers = tool_named("find_callers").description.unwrap();
        for needle in [
            "provenance 'lsp-live'",
            "'lsp-indexed' when pre-computed by `knot-lite index`",
            "provenance 'heuristic'",
            "Every target states its provenance.",
            "language-server answers are limited to the first 10 targets per call",
        ] {
            assert!(callers.contains(needle), "{needle} missing in {callers}");
        }
        assert!(
            !callers.contains("language servers are not used yet"),
            "the Phase 2 wording is stale"
        );
        let status = tool_named("status").description.unwrap();
        assert!(
            status.ends_with(
                "For tier-3 languages it also shows whether the language server is running, its version and memory use, and whether language-server results were pre-computed."
            ),
            "{status}"
        );
    }

    #[test]
    fn path_fields_describe_knot_globs() {
        for name in ["list_files", "search_hybrid_context"] {
            let tool = tool_named(name);
            let description = property(&tool, "path")["description"]
                .as_str()
                .unwrap()
                .to_owned();
            assert!(
                description.starts_with(
                    "Optional repo-relative directory prefix (e.g. 'src/api', matched on a path boundary) or glob pattern (segments: `*` segment wildcard, `**` any depth, `?` one char)."
                ),
                "{name}: {description}"
            );
            assert!(!description.contains("not supported"), "{description}");
            let tool_description = tool.description.unwrap();
            assert!(
                !tool_description.contains("rejected"),
                "{name}: {tool_description}"
            );
        }
    }

    #[test]
    fn find_callers_describes_knot_tier_ladder() {
        let callers = tool_named("find_callers").description.unwrap();
        assert!(
            callers.contains(
                "exact name → signature prefix (queries containing '(', e.g. `accept(List`) → name prefix (queries under 4 characters, case-insensitive) → fuzzy substring (queries of 4+ characters"
            ),
            "{callers}"
        );
        assert!(
            !callers.contains("not available in this version"),
            "{callers}"
        );
    }

    #[test]
    fn all_tools_have_input_schema() {
        for tool in all_tools() {
            assert_eq!(tool.input_schema.type_(), "object");
        }
    }

    #[test]
    fn required_fields_match_appendix_a() {
        let required = |name: &str| -> Vec<String> {
            schema_map(&tool_named(name))
                .get("required")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        };
        assert_eq!(required("search_hybrid_context"), vec!["query".to_string()]);
        assert_eq!(required("find_callers"), vec!["entity_name".to_string()]);
        assert_eq!(required("explore_file"), vec!["file_path".to_string()]);
        assert!(required("list_files").is_empty());
        assert!(required("status").is_empty());
    }

    #[test]
    fn all_tools_have_optional_repo_name() {
        for tool in all_tools() {
            let prop = property(&tool, "repo_name");
            let desc = prop
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default();
            assert!(desc.contains("'all'"), "{}: {desc}", tool.name);
            assert!(desc.contains("comma-separated"), "{}: {desc}", tool.name);
        }
    }

    #[test]
    fn schema_constraints_match_appendix_a() {
        let search = tool_named("search_hybrid_context");
        let max_results = property(&search, "max_results");
        assert_eq!(max_results.get("minimum").and_then(Value::as_i64), Some(1));
        assert_eq!(
            max_results.get("maximum").and_then(Value::as_i64),
            Some(100)
        );
        assert_eq!(max_results.get("default").and_then(Value::as_i64), Some(5));
        let query = property(&search, "query");
        assert_eq!(query.get("minLength").and_then(Value::as_u64), Some(1));
        assert_eq!(query.get("maxLength").and_then(Value::as_u64), Some(500));

        let callers = tool_named("find_callers");
        let kinds = property(&callers, "kinds");
        assert_eq!(kinds.get("maxLength").and_then(Value::as_u64), Some(1024));
        let max_targets = property(&callers, "max_targets");
        assert_eq!(max_targets.get("minimum").and_then(Value::as_i64), Some(1));
        assert_eq!(
            max_targets.get("maximum").and_then(Value::as_i64),
            Some(500)
        );
        assert_eq!(max_targets.get("default").and_then(Value::as_i64), Some(25));
        let entity_name = property(&callers, "entity_name");
        assert_eq!(
            entity_name.get("minLength").and_then(Value::as_u64),
            Some(1)
        );
        assert_eq!(
            entity_name.get("maxLength").and_then(Value::as_u64),
            Some(255)
        );

        let explore = tool_named("explore_file");
        let file_path = property(&explore, "file_path");
        assert_eq!(file_path.get("minLength").and_then(Value::as_u64), Some(1));

        let files = tool_named("list_files");
        let path = property(&files, "path");
        assert_eq!(path.get("maxLength").and_then(Value::as_u64), Some(500));
    }

    #[test]
    fn parse_args_rejects_missing_required() {
        let params = call_params("find_callers", serde_json::Map::new());
        let err = parse_args::<FindCallersTool>("find_callers", &params, "demo").unwrap_err();
        assert!(
            err.to_string()
                .starts_with("invalid arguments for find_callers"),
            "{}",
            err
        );
    }

    #[test]
    fn parse_args_accepts_repo_name_array_of_self() {
        let mut args = serde_json::Map::new();
        args.insert("entity_name".to_string(), json!("E"));
        args.insert("repo_name".to_string(), json!(["demo", "all"]));
        let params = call_params("find_callers", args);
        let tool: FindCallersTool = parse_args("find_callers", &params, "demo").expect("parse");
        assert_eq!(tool.entity_name, "E");
    }

    #[test]
    fn parse_args_rejects_foreign_repo() {
        let mut args = serde_json::Map::new();
        args.insert("entity_name".to_string(), json!("E"));
        args.insert("repo_name".to_string(), json!("other"));
        let params = call_params("find_callers", args);
        let err = parse_args::<FindCallersTool>("find_callers", &params, "demo").unwrap_err();
        assert!(err.to_string().contains("not served here"), "{err}");
    }

    #[tokio::test]
    async fn query_tool_not_indexed_message() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = crate::config::test_support::config_for(tmp.path().to_path_buf());
        let handler = KnotLiteMcpHandler::new(cfg);
        let mut args = serde_json::Map::new();
        args.insert("entity_name".to_string(), json!("foo"));
        let params = call_params("find_callers", args);
        let err = query_tool::<FindCallersTool, _>(
            "find_callers",
            params,
            &handler,
            QueryToolSpec {
                to_params: crate::models::params::CallersParams::from_tool,
                run: crate::cli_tools::run_find_callers,
                format: crate::cli_tools::format_callers_output,
            },
        )
        .await
        .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("has no index"), "{text}");
        assert!(text.contains("`status`"), "{text}");
    }

    /// A handler whose runners never touch a repository (dry-run configs are
    /// refused by `config()`, so this one serves an empty temp directory).
    fn handler_for_runner_tests() -> (tempfile::TempDir, KnotLiteMcpHandler) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = crate::config::test_support::config_for(tmp.path().to_path_buf());
        (tmp, KnotLiteMcpHandler::new(cfg))
    }

    fn find_callers_params() -> CallToolRequestParams {
        let mut args = serde_json::Map::new();
        args.insert("entity_name".to_string(), json!("x"));
        call_params("find_callers", args)
    }

    fn slow_runner(
        _cfg: &Config,
        _services: &QueryServices,
        _params: &(),
    ) -> anyhow::Result<Value> {
        std::thread::sleep(std::time::Duration::from_millis(400));
        Ok(json!({"ok": true}))
    }

    fn panicking_runner(
        _cfg: &Config,
        _services: &QueryServices,
        _params: &(),
    ) -> anyhow::Result<Value> {
        panic!("the runner panicked");
    }

    fn unit_spec(
        run: fn(&Config, &QueryServices, &()) -> anyhow::Result<Value>,
    ) -> QueryToolSpec<FindCallersTool, ()> {
        QueryToolSpec {
            to_params: |_| (),
            run,
            format: |_, _| String::new(),
        }
    }

    #[test]
    fn query_tools_run_on_blocking_pool() {
        // Source-level guard: the runner is handed to `spawn_blocking`, never called inline.
        let source = include_str!("mod.rs");
        let start = source
            .find("pub(crate) async fn query_tool")
            .expect("query_tool");
        let end = source[start..].find("/// Generates").expect("end") + start;
        let body = &source[start..end];
        assert!(body.contains("spawn_blocking"), "{body}");
        assert!(
            !body.contains("(spec.run)("),
            "the runner must not run on the async thread"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_slow_query_does_not_block_the_async_runtime() {
        let (_tmp, handler) = handler_for_runner_tests();
        let started = std::time::Instant::now();
        let (result, ticked_after) = tokio::join!(
            query_tool::<FindCallersTool, _>(
                "find_callers",
                find_callers_params(),
                &handler,
                unit_spec(slow_runner),
            ),
            async {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                started.elapsed()
            }
        );
        assert!(result.is_ok());
        // On a single-threaded runtime the timer can only fire while the
        // runner is NOT occupying the thread.
        assert!(
            ticked_after < std::time::Duration::from_millis(300),
            "the runtime was blocked for {ticked_after:?}"
        );
    }

    #[tokio::test]
    async fn a_panicking_runner_becomes_a_tool_error() {
        let (_tmp, handler) = handler_for_runner_tests();
        let err = query_tool::<FindCallersTool, _>(
            "find_callers",
            find_callers_params(),
            &handler,
            unit_spec(panicking_runner),
        )
        .await
        .unwrap_err();
        let text = err.to_string();
        assert!(text.starts_with("find_callers: internal error:"), "{text}");
    }
}
