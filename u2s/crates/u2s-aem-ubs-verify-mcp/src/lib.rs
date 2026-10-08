//! The UBS AEM verifier as a library: the UBS form driver and the tool specs,
//! which a host combines with `u2s_aem_verify_core::server::ServerConfig` to
//! run the verifier in-process instead of over stdio. `src/main.rs` builds the
//! stdio server from the same parts; its module doc explains what the driver
//! does differently for a UBS form.

pub mod driver;
pub mod specs;
mod ubs_js;
mod ubs_metadata;
