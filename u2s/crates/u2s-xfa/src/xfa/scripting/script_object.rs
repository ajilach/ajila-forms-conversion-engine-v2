//! XFA Named Script Object Wrapper
//!
//! Per XFA 3.3 §10 pp. 376–378, named script objects (`<variables><script>`)
//! are compiled into script objects where **all** top-level declarations
//! (variables and functions) become properties/methods of the object.
//!
//! ## XFA Spec Example (p. 377):
//! ```xml
//! <variables>
//!   <script name="foo" contentType="application/x-javascript">
//!     var a = 0;
//!     var b = 0;
//!     var factor = 1;
//!     function sum(val1, val2) {
//!       var sum = (val1 + val2) * this.factor;
//!       return (sum);
//!     }
//!   </script>
//! </variables>
//! ```
//!
//! After instantiation, other scripts can:
//! - Read/write properties: `foo.a = 2; foo.factor = 4;`
//! - Call methods: `foo.sum(foo.a, foo.b);`
//! - Use `this` inside methods to refer to the script object: `this.factor`
//!
//! ## Implementation
//!
//! We wrap the script content in an IIFE that:
//! 1. Executes the original content in a closure scope
//! 2. Creates `_obj` and exposes all top-level vars via `Object.defineProperty`
//!    with getter/setter pairs that sync with the closure variables
//! 3. Assigns all top-level functions as methods on `_obj` (so `this` === `_obj`)
//! 4. Returns `_obj` as the named global variable
//!
//! The getter/setter approach ensures bidirectional sync: external writes like
//! `foo.a = 42` update the closure variable, so internal functions that reference
//! `a` directly (without `this.`) see the updated value.

use regex_lite::Regex;

/// Regex for top-level `function name(` declarations.
/// Matches `function someName(` at the start of a line (with optional whitespace).
fn re_function() -> Regex {
    Regex::new(r"(?m)^\s*function\s+([a-zA-Z_$][a-zA-Z0-9_$]*)\s*\(").unwrap()
}

/// Regex for top-level `var name` declarations.
/// Matches `var name`, `var name =`, `var name,` etc. at the start of a line.
fn re_var() -> Regex {
    Regex::new(r"(?m)^\s*var\s+([a-zA-Z_$][a-zA-Z0-9_$]*)").unwrap()
}

/// Regex for bare assignments at the start of a line: `name = ...`
/// These are globals in the original script but should become closure-scoped properties.
/// Excludes lines that start with keywords like `if`, `for`, `return`, `function`, `var`, etc.
fn re_bare_assign() -> Regex {
    Regex::new(r"(?m)^([a-zA-Z_$][a-zA-Z0-9_$]*)\s*=\s*").unwrap()
}

/// JavaScript keywords that should not be treated as bare assignments.
const JS_KEYWORDS: &[&str] = &[
    "break",
    "case",
    "catch",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "finally",
    "for",
    "function",
    "if",
    "in",
    "instanceof",
    "new",
    "return",
    "switch",
    "this",
    "throw",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "class",
    "const",
    "enum",
    "export",
    "extends",
    "import",
    "super",
    "implements",
    "interface",
    "let",
    "package",
    "private",
    "protected",
    "public",
    "static",
    "yield",
    "true",
    "false",
    "null",
    "undefined",
];

/// Extract top-level function names from script content.
fn extract_function_names(content: &str) -> Vec<String> {
    re_function()
        .captures_iter(content)
        .filter_map(|cap| cap.get(1).map(|m| m.as_str().to_string()))
        .collect()
}

/// Extract top-level `var` declaration names from script content.
fn extract_var_names(content: &str) -> Vec<String> {
    re_var()
        .captures_iter(content)
        .filter_map(|cap| cap.get(1).map(|m| m.as_str().to_string()))
        .collect()
}

/// Extract bare assignment names (e.g., `myEN = {...}`) from script content.
/// Filters out JavaScript keywords and names that are already declared as `var` or `function`.
fn extract_bare_assignment_names(
    content: &str,
    var_names: &[String],
    function_names: &[String],
) -> Vec<String> {
    re_bare_assign()
        .captures_iter(content)
        .filter_map(|cap| {
            let name = cap.get(1)?.as_str();
            // Skip JS keywords
            if JS_KEYWORDS.contains(&name) {
                return None;
            }
            // Skip names already declared as var or function
            if var_names.iter().any(|v| v == name) || function_names.iter().any(|f| f == name) {
                return None;
            }
            Some(name.to_string())
        })
        .collect()
}

/// Generate the JavaScript wrapper for a named XFA script object.
///
/// Per XFA 3.3 §10 pp. 376–378, the script content is compiled into a script
/// object where all top-level variables and functions become properties/methods.
///
/// # Arguments
/// * `name` - The script object name (e.g., "foo", "soGlobal")
/// * `content` - The raw JavaScript content from `<variables><script>`
/// * `use_global_this` - If true, uses `globalThis.{name}` instead of `var {name}`
///
/// # Returns
/// The wrapped JavaScript source code ready for execution.
pub fn wrap_script_object(name: &str, content: &str, use_global_this: bool) -> String {
    let function_names = extract_function_names(content);
    let var_names = extract_var_names(content);
    let bare_names = extract_bare_assignment_names(content, &var_names, &function_names);

    // NOTE: We do NOT prepend `var` to bare assignments. In XFA forms, bare
    // assignments like `myIT = {...}` intentionally leak to global scope so
    // that other scripts can access them directly (e.g., `myIT.GV_FamilyName`).
    // We keep the original content as-is and expose bare assignments on the
    // script object via getter/setter that reads/writes the global.

    // `var` declarations need closure-scoped getter/setter
    let mut property_defs = String::new();
    for var_name in &var_names {
        property_defs.push_str(&format!(
            r#"
                    Object.defineProperty(_obj, '{var_name}', {{
                        get: function() {{ return {var_name}; }},
                        set: function(_v) {{ {var_name} = _v; }},
                        enumerable: true,
                        configurable: true
                    }});"#,
        ));
    }

    // Bare assignments are globals — expose them on the object via global read/write.
    // This allows both `myIT.GV_X` (global) and `soObj.myIT.GV_X` (property) to work.
    for bare_name in &bare_names {
        property_defs.push_str(&format!(
            r#"
                    Object.defineProperty(_obj, '{bare_name}', {{
                        get: function() {{ return {bare_name}; }},
                        set: function(_v) {{ {bare_name} = _v; }},
                        enumerable: true,
                        configurable: true
                    }});"#,
        ));
    }

    // Build function assignments (assigning as method makes `this` === `_obj` when called as _obj.fn())
    let mut func_assigns = String::new();
    for func_name in &function_names {
        func_assigns.push_str(&format!(
            "\n                    _obj.{func_name} = {func_name};",
        ));
    }

    // Build the IIFE wrapper
    let assignment = if use_global_this {
        format!("globalThis.{name}")
    } else {
        format!("var {name}")
    };

    format!(
        r#"
                {assignment} = (function() {{
                    {content}
                    var _obj = {{}};{property_defs}{func_assigns}
                    return _obj;
                }})();
                "#,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_function_names() {
        let content = r#"
            var a = 0;
            function sum(val1, val2) { return val1 + val2; }
            function formatCurrency(s) { return "$" + s; }
            // function commented() { }
            var b = function() {}; // not a declaration
        "#;
        let names = extract_function_names(content);
        assert_eq!(names, vec!["sum", "formatCurrency"]);
    }

    #[test]
    fn test_extract_var_names() {
        let content = r#"
            var a = 0;
            var b = 0;
            var factor = 1;
            function sum(val1, val2) {
                var inner = val1 + val2; // nested var — included but harmless
                return inner;
            }
        "#;
        let names = extract_var_names(content);
        assert!(names.contains(&"a".to_string()));
        assert!(names.contains(&"b".to_string()));
        assert!(names.contains(&"factor".to_string()));
    }

    #[test]
    fn test_extract_bare_assignments() {
        let content = r#"
myEN = { key: "value" };
showBarcode = true;
var a = 0;
function sum() {}
if (true) { x = 1; }
        "#;
        let vars = extract_var_names(content);
        let funcs = extract_function_names(content);
        let bare = extract_bare_assignment_names(content, &vars, &funcs);
        assert!(bare.contains(&"myEN".to_string()));
        assert!(bare.contains(&"showBarcode".to_string()));
        // `a` is a var, `sum` is a function, `if` is a keyword — none should appear
        assert!(!bare.contains(&"a".to_string()));
        assert!(!bare.contains(&"sum".to_string()));
    }

    #[test]
    fn test_wrap_script_object_xfa_spec_example() {
        // XFA 3.3 §10 p.377 example
        let content = r#"
var a = 0;
var b = 0;
var factor = 1;
function sum(val1, val2) {
    var s = (val1 + val2) * this.factor;
    return s;
}
        "#;
        let wrapped = wrap_script_object("foo", content, false);

        // Should contain the assignment
        assert!(wrapped.contains("var foo = (function()"));
        // Should contain getter/setter for a, b, factor
        assert!(wrapped.contains("Object.defineProperty(_obj, 'a'"));
        assert!(wrapped.contains("Object.defineProperty(_obj, 'b'"));
        assert!(wrapped.contains("Object.defineProperty(_obj, 'factor'"));
        // Should assign sum as method
        assert!(wrapped.contains("_obj.sum = sum"));
        // Should NOT contain any hardcoded event names
        assert!(!wrapped.contains("setupVariables"));
        assert!(!wrapped.contains("calculate"));
        assert!(!wrapped.contains("validate"));
    }

    #[test]
    fn test_wrap_script_object_bare_assignments() {
        let content = r#"
myEN = { key: "value" };
showBarcode = true;
function getPagination(cp, tp) { return cp + "/" + tp; }
        "#;
        let wrapped = wrap_script_object("soGlobal", content, false);

        // Bare assignments should remain as-is (global scope), NOT converted to var
        // This is critical: other scripts access them directly (e.g., `myIT.GV_X`)
        assert!(wrapped.contains("myEN = { key: \"value\" };"));
        assert!(!wrapped.contains("var myEN"));
        // Should have getter/setter for both on the object
        assert!(wrapped.contains("Object.defineProperty(_obj, 'myEN'"));
        assert!(wrapped.contains("Object.defineProperty(_obj, 'showBarcode'"));
        // Should assign function
        assert!(wrapped.contains("_obj.getPagination = getPagination"));
    }

    #[test]
    fn test_wrap_script_object_global_this() {
        let content = "var x = 1;";
        let wrapped = wrap_script_object("myObj", content, true);
        assert!(wrapped.contains("globalThis.myObj = (function()"));
    }

    #[test]
    fn test_wrap_script_object_empty_content() {
        let wrapped = wrap_script_object("empty", "", false);
        assert!(wrapped.contains("var empty = (function()"));
        assert!(wrapped.contains("return _obj;"));
    }
}
