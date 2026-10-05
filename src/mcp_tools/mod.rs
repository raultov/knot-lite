//! Thin MCP tool wrappers over the `cli_tools::run_*` functions.
//!
//! Every tool file declares the `#[mcp_tool]` input struct (Appendix A) and a
//! one-line `handle` that delegates here; the shared machinery keeps each tool
//! file tiny and the diagnostics identical.

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
pub(crate) fn query_tool<T, P>(
    tool: &str,
    params: CallToolRequestParams,
    handler: &KnotLiteMcpHandler,
    spec: QueryToolSpec<T, P>,
) -> Result<CallToolResult, CallToolError>
where
    T: DeserializeOwned,
{
    let cfg = handler.config()?.clone();
    let args = parse_args::<T>(tool, &params, &cfg.repo_name)?;
    match (spec.run)(&cfg, &handler.services, &(spec.to_params)(args)) {
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

    #[test]
    fn query_tool_not_indexed_message() {
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
        .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("has no index"), "{text}");
        assert!(text.contains("`status`"), "{text}");
    }
}
