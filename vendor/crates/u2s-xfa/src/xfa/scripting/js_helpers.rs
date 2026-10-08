//! JavaScript Helper Code
//!
//! This module contains JavaScript helper code that is injected into the
//! scripting environment to provide XFA-specific functionality.

/// Global SOM resolution helper function.
///
/// When a path like "Page.SectionTitle.STP_SectionTitle.ffrb1" is accessed,
/// JavaScript property chain works for subforms but fails for floating fields.
/// This helper provides fallback resolution.
pub const XFA_RESOLVE_PATH_HELPER: &str = r#"
function _xfa_resolve_path_(path) {
    var parts = path.split('.');
    var obj = this; // Start from global
    
    // Try to traverse the path
    for (var i = 0; i < parts.length; i++) {
        var part = parts[i];
        if (obj && typeof obj[part] !== 'undefined') {
            obj = obj[part];
        } else {
            // Path traversal failed - try looking up the last part in the registry
            var lastPart = parts[parts.length - 1];
            if (typeof _xfa_fields_ !== 'undefined' && _xfa_fields_[lastPart]) {
                return _xfa_fields_[lastPart];
            }
            return null;
        }
    }
    return obj;
}
"#;

/// Combined JavaScript helpers for XFA environment setup.
pub fn get_all_helpers() -> String {
    XFA_RESOLVE_PATH_HELPER.to_string()
}
