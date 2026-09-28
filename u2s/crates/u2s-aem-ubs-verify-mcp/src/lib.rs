//! Library face of the UBS AEM verifier, for hosts that run it in-process
//! instead of over stdio: the UBS form driver and the tool specs, which a host
//! combines with `u2s_aem_verify_core::server::ServerConfig`. Local to the
//! blueprint vendoring, see u2s/VENDORED.md.

pub mod driver;
pub mod specs;
mod ubs_js;
mod ubs_metadata;
