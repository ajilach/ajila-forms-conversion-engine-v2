//! The stdio binary of [`u2s_render_pdf_mcp`]: the PDF render server,
//! configured from the environment and served over stdio. The server itself is
//! the library, which a host can also run in-process.

use rmcp::{ServiceExt, transport::stdio};
use u2s_render_pdf_mcp::PdfRenderServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // Bind pdfium before serving. A render server that starts without its
    // renderer produces confusing per-call errors forever; refusing to start
    // produces one clear error once.
    let server = match PdfRenderServer::new() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("u2s-render-pdf-mcp: cannot start: {e}");
            eprintln!(
                "hint: run scripts/fetch-pdfium.sh, or set PDFIUM_LIB_PATH to the directory \
                 containing the pdfium dynamic library"
            );
            std::process::exit(2);
        }
    };

    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
