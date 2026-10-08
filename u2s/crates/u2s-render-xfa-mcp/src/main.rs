//! The stdio binary of [`u2s_render_xfa_mcp`]: the XFA render server,
//! configured from the environment and served over stdio. The server itself is
//! the library, which a host can also run in-process.

use rmcp::{ServiceExt, transport::stdio};
use u2s_render_xfa_mcp::XfaRenderServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let server = match XfaRenderServer::new() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("u2s-render-xfa-mcp: cannot start: {e}");
            eprintln!(
                "hint: set U2S_FONT_DIR to a directory of .ttf/.otf files, or run \
                 scripts/fetch-fonts.sh. XFA layout cannot measure text without fonts, and \
                 would silently produce wrong page breaks rather than failing."
            );
            std::process::exit(2);
        }
    };

    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
