//! The in-process constructor refuses to start without fonts, as the stdio
//! binary does: layout without them silently measures text wrong. Its own
//! test binary, because font registration is process-global and every other
//! test registers them.

use u2s_render_xfa::{BlobStore, Limits};
use u2s_render_xfa_mcp::XfaRenderServer;

#[test]
fn with_parts_refuses_to_start_without_fonts() {
    let blobs = BlobStore::new(std::env::temp_dir().join("u2s-render-xfa-mcp-without-fonts"));
    let refused = XfaRenderServer::with_parts(Limits::default(), blobs);
    assert!(refused.is_err(), "a fontless server must not start");
}
