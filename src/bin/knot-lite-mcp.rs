//! `knot-lite-mcp` stdio MCP server binary.

use clap::Parser as _;
use rust_mcp_sdk::error::SdkResult;
use rust_mcp_sdk::mcp_server::{McpServerOptions, ToMcpServerHandler, server_runtime};
use rust_mcp_sdk::{McpServer, TransportOptions};

use knot_lite::config::{CommonArgs, Config, EnvLayers, LOG_ENV, resolve_dry_run};
use knot_lite::mcp_handler::{KnotLiteMcpHandler, build_server_details};
use knot_lite::mcp_transport::StdioServerTransport;
use knot_lite::utils;

#[derive(clap::Parser)]
#[command(
    name = "knot-lite-mcp",
    version,
    about = "knot-lite MCP server (stdio, MCP 2026-07-28)"
)]
struct McpCli {
    /// Shared repository flags (`--repo-path`, `--repo-name`).
    #[command(flatten)]
    common: CommonArgs,
    /// Answer protocol requests without opening the repository (quality checks).
    /// `KNOT_LITE_DRY_RUN` (process environment or `.env`) takes on/off,
    /// true/false, yes/no or 1/0; it is read by `resolve_dry_run`, not by clap.
    #[arg(long, hide = true)]
    dry_run: bool,
}

#[tokio::main]
async fn main() -> SdkResult<()> {
    let env = EnvLayers::load();
    utils::init_logging(utils::MCP_LOG_LEVEL, env.get(LOG_ENV).as_deref()).expect("logging");
    let cli = McpCli::parse();
    let (dry_run, warning) = resolve_dry_run(cli.dry_run, &|key| env.get(key));
    if let Some(warning) = warning {
        tracing::warn!("{warning}");
    }
    let handler = if dry_run {
        KnotLiteMcpHandler::new_dry_run()
    } else {
        let cfg = Config::load_mcp(&cli.common, false).unwrap_or_else(|e| {
            eprintln!("knot-lite-mcp: {e:#}");
            std::process::exit(1);
        });
        tracing::info!(
            repo = %cfg.repo_name,
            path = %cfg.repo_path.display(),
            "knot-lite MCP server starting"
        );
        KnotLiteMcpHandler::new(cfg)
    };
    let transport = StdioServerTransport::new(TransportOptions::default());
    let server = server_runtime::create_server(McpServerOptions {
        server_details: build_server_details(),
        transport,
        handler: handler.to_mcp_server_handler(),
        message_observer: None,
    });
    if let Err(e) = server.start().await {
        eprintln!("{}", e.rpc_error_message().unwrap_or(&e.to_string()));
    }
    Ok(())
}
