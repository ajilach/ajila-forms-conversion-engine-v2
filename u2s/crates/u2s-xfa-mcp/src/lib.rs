//! Library face of the XFA data server, for hosts that run it in-process
//! instead of over stdio. `main.rs` is compiled into it unchanged apart from
//! visibility, so the stdio binary and the library cannot drift. Local to the
//! blueprint vendoring, see u2s/VENDORED.md.

#[allow(dead_code)]
#[path = "main.rs"]
mod server;

pub use server::XfaDataServer;
pub use server::specs;
