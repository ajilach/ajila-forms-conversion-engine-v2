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

/// `src` with every comment and the inside of every string and regular
/// expression literal replaced by spaces, newlines kept, so that a scan for
/// top-level declarations sees only code, at the same offsets and lines: a
/// `function setupVariables()` inside `/* ... */` (AAHQ_019_DE) is no
/// declaration.
pub fn blank_comments_and_strings(src: &str) -> String {
    #[derive(PartialEq)]
    enum State {
        Code,
        Line,
        Block,
        Quote(char),
        Regex { in_class: bool },
    }
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut state = State::Code;
    // The last character of code that was not whitespace: a `/` after one of
    // these (or at the start) begins a regular expression, not a division.
    let mut last_significant: Option<char> = None;
    let mut i = 0;
    let blank = |c: char| if c == '\n' { '\n' } else { ' ' };
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match state {
            State::Code => {
                if c == '/' && next == Some('/') {
                    state = State::Line;
                    out.push_str("  ");
                    i += 2;
                    continue;
                }
                if c == '/' && next == Some('*') {
                    state = State::Block;
                    out.push_str("  ");
                    i += 2;
                    continue;
                }
                if c == '"' || c == '\'' || c == '`' {
                    state = State::Quote(c);
                    out.push(c);
                } else if c == '/'
                    && last_significant.is_none_or(|p| "(,=:[!&|?{};+-*%<>~^".contains(p))
                {
                    state = State::Regex { in_class: false };
                    out.push(c);
                } else {
                    out.push(c);
                }
                if !c.is_whitespace() {
                    last_significant = Some(c);
                }
            }
            State::Line => {
                if c == '\n' {
                    state = State::Code;
                }
                out.push(blank(c));
            }
            State::Block => {
                if c == '*' && next == Some('/') {
                    state = State::Code;
                    out.push_str("  ");
                    i += 2;
                    continue;
                }
                out.push(blank(c));
            }
            State::Quote(q) => {
                if c == '\\' {
                    out.push(' ');
                    if let Some(n) = next {
                        out.push(blank(n));
                    }
                    i += 2;
                    continue;
                }
                if c == q {
                    state = State::Code;
                    out.push(c);
                    last_significant = Some(c);
                } else {
                    out.push(blank(c));
                }
            }
            State::Regex { in_class } => {
                if c == '\\' {
                    out.push(' ');
                    if let Some(n) = next {
                        out.push(blank(n));
                    }
                    i += 2;
                    continue;
                }
                if c == '\n' {
                    // Not a regular expression after all; resume as code.
                    state = State::Code;
                    out.push(c);
                } else if c == '/' && !in_class {
                    state = State::Code;
                    out.push(c);
                    last_significant = Some(c);
                } else {
                    if c == '[' {
                        state = State::Regex { in_class: true };
                    } else if c == ']' {
                        state = State::Regex { in_class: false };
                    }
                    out.push(blank(c));
                }
            }
        }
        i += 1;
    }
    out
}

/// The factory of a named XFA script object (XFA 3.3 §10 pp. 376-378): an
/// expression for a function that runs `content` and returns the script
/// object, every top-level `var` and bare assignment a property and every
/// top-level function a method. The engine calls it on the first reference
/// to the object (see `js_helpers::XFA_SCRIPT_OBJECTS`).
///
/// Declarations are read from the code alone (comments and strings
/// blanked), and a function is exported only when it really is defined, so
/// a nested or commented-out declaration never breaks the object.
pub fn script_object_factory(content: &str) -> String {
    let code = blank_comments_and_strings(content);
    let function_names = extract_function_names(&code);
    let var_names = extract_var_names(&code);
    let bare_names = extract_bare_assignment_names(&code, &var_names, &function_names);

    // Bare assignments like `myIT = {...}` stay globals, as in Acrobat: other
    // scripts read them directly (`myIT.GV_FamilyName`). They are exposed on
    // the object as well, through the global.
    let mut property_defs = String::new();
    for name in var_names.iter().chain(&bare_names) {
        property_defs.push_str(&format!(
            r#"
                    Object.defineProperty(_obj, '{name}', {{
                        get: function() {{ return {name}; }},
                        set: function(_v) {{ {name} = _v; }},
                        enumerable: true,
                        configurable: true
                    }});"#,
        ));
    }

    // Assigned as methods, so `this` is the object when called as `so.fn()`.
    let mut func_assigns = String::new();
    for name in &function_names {
        func_assigns.push_str(&format!(
            "\n                    if (typeof {name} === 'function') _obj.{name} = {name};",
        ));
    }

    format!(
        r#"(function() {{
                    {content}
                    var _obj = {{}};{property_defs}{func_assigns}
                    return _obj;
                }})"#,
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
    fn the_factory_exposes_vars_and_methods_of_the_spec_example() {
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
        let factory = script_object_factory(content);
        assert!(factory.starts_with("(function() {"));
        assert!(factory.contains("Object.defineProperty(_obj, 'a'"));
        assert!(factory.contains("Object.defineProperty(_obj, 'factor'"));
        assert!(factory.contains("if (typeof sum === 'function') _obj.sum = sum;"));
    }

    #[test]
    fn bare_assignments_stay_globals_and_are_exposed() {
        let content = "myEN = { key: \"value\" };\nshowBarcode = true;\n";
        let factory = script_object_factory(content);
        assert!(factory.contains("myEN = { key: \"value\" };"));
        assert!(!factory.contains("var myEN"));
        assert!(factory.contains("Object.defineProperty(_obj, 'myEN'"));
    }

    #[test]
    fn a_function_in_a_block_comment_or_a_string_is_not_exported() {
        let content = "/*my66378D = {};\nfunction setupVariables()\n{\n}*/\nvar s = \"\nfunction quoted(\";\nfunction real() {}\n";
        let factory = script_object_factory(content);
        assert!(!factory.contains("_obj.setupVariables"), "{factory}");
        assert!(!factory.contains("_obj.quoted"));
        assert!(!factory.contains("'my66378D'"));
        assert!(factory.contains("_obj.real = real"));
    }

    #[test]
    fn blanking_keeps_offsets_lines_and_division() {
        let src = "a = b / c; // x\nr = /\\/[/]/g; s = 'it''s'; /* y\n z */ t = 1;";
        let blank = blank_comments_and_strings(src);
        assert_eq!(blank.chars().count(), src.chars().count());
        assert_eq!(blank.lines().count(), src.lines().count());
        assert!(blank.starts_with("a = b / c;"), "{blank}");
        assert!(blank.contains("t = 1;"));
        assert!(!blank.contains('x') && !blank.contains('y'));
    }
}
