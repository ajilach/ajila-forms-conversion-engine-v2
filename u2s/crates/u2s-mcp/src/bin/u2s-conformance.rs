//! `u2s-conformance` — the CLI a third-party author runs against their own
//! server before registering it (PLAN.md), and what `POST
//! /v1/mcp-tools/{id}/conformance` runs in-process once the app exists (step
//! 4 of `plan-the-agent-engine.md`; this binary is the standalone shape of
//! the same call).
//!
//! ```text
//! u2s-conformance --fixtures /path/to/fixtures -- /path/to/server-binary [args...]
//! ```
//!
//! Exit code 0 on a passing report, 1 on a failing one, 2 on a usage or
//! connection error — distinct codes so a CI step can tell "the server is
//! non-conformant" from "I invoked this wrong".

use std::path::PathBuf;
use std::process::ExitCode;

use u2s_mcp::pool::{McpClientPool, Transport};

fn usage() -> ! {
    eprintln!(
        "usage: u2s-conformance --fixtures <dir> -- <server-binary> [args...]\n\n\
         Runs the PLAN.md conformance suite against a stdio MCP server and \
         prints the report. Exit 0 = passed, 1 = failed, 2 = usage/connection error."
    );
    std::process::exit(2);
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let Some(fixtures_flag) = args.iter().position(|a| a == "--fixtures") else {
        usage();
    };
    let Some(fixture_dir) = args.get(fixtures_flag + 1) else {
        usage();
    };
    let fixture_dir = PathBuf::from(fixture_dir);

    let Some(separator) = args.iter().position(|a| a == "--") else {
        usage();
    };
    let Some(command) = args.get(separator + 1) else {
        usage();
    };
    let command = PathBuf::from(command);
    let server_args: Vec<String> = args[separator + 2..].to_vec();

    let transport = Transport::Stdio {
        command,
        args: server_args,
        env: Vec::new(),
    };
    let pool = McpClientPool::new();

    let report =
        match u2s_mcp::conformance::run_via_pool(&pool, "cli", &transport, &fixture_dir).await {
            Ok(report) => report,
            Err(e) => {
                eprintln!("could not run conformance: {e}");
                return ExitCode::from(2);
            }
        };

    pool.shutdown().await;

    if report.findings.is_empty() {
        println!("PASSED — no findings");
    }
    for finding in &report.findings {
        println!("[{:?}] {}", finding.severity, finding.message);
    }

    if report.passed() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
