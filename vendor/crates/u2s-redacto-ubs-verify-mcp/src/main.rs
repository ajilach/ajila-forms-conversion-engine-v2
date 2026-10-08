//! The stdio binary of [`u2s_redacto_ubs_verify_mcp`]: the Redacto UBS verifier,
//! configured from the environment and served over stdio. The server itself is
//! the library, which a host can also run in-process.

use rmcp::{ServiceExt, transport::stdio};
use u2s_redacto_ubs_verify_mcp::RedactoVerifyServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let server = RedactoVerifyServer::new();
    server.start_housekeeping().await;
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
