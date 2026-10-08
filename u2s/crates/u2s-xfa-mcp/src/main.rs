//! The stdio binary of [`u2s_xfa_mcp`]: the raw XFA data server,
//! configured from the environment and served over stdio. The server itself is
//! the library, which a host can also run in-process.

use rmcp::{ServiceExt, transport::stdio};
use u2s_xfa_mcp::XfaDataServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let server = XfaDataServer;
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
