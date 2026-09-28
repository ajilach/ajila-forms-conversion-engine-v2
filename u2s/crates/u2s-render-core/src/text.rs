//! Re-exports [`u2s_core::text`] unchanged — see that module's docs for why
//! this moved. `u2s-render-xfa` and `u2s-xfa-mcp` both reach it through
//! `u2s_render_core::window_chars`, never this module path directly, so
//! this re-export is what keeps them at zero diff.

pub use u2s_core::text::{Grep, GrepMatch, Pattern, Windowed, window_chars};
