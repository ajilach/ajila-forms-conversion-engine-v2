//! FormCalc, translated to JavaScript (XFA 3.3 Chapter 25, "FormCalc
//! Specification").
//!
//! A script with no `contentType` is FormCalc. This supports the language's
//! expression core: literals, the operators of "Basic Expressions" with
//! their precedence (`|`/`or`, `&`/`and`, `==`/`<>`/`eq`/`ne`, `<`/`<=`/`>`/
//! `>=`/`lt`/`le`/`gt`/`ge`, `+`/`-`, `*`/`/`, unary `-`/`+`/`not`),
//! assignment, `if ... then ... elseif ... else ... endif`, and accessors:
//! `$`, `$form`, `$layout`, `$host`, `$event`, `$xfa`, dotted names and
//! `[n]` indices. Everything else (loops, `var`, `func`, function and method
//! calls, `..`, `.#`, `[*]`, `$data`, `$record`, `$template`, `!names`) is
//! refused with the construct and its line, so a script either runs as
//! FormCalc would or is reported as not run.
//!
//! The JavaScript runs inside the script's own scope (see the engine's
//! `execute_javascript`), so unqualified names resolve by the XFA 3.3 §3
//! scope walk as FormCalc's own names do. FormCalc is value-oriented
//! ("Using SOM Expressions in FormCalc"): an operand that names a node is
//! that node's value. The runtime helpers (`_fc_*`) are in
//! `js_helpers::XFA_FORMCALC_RUNTIME`.

use std::fmt;

/// Why a FormCalc script could not be translated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormCalcError {
    /// 1-based line of the script the problem is on.
    pub line: usize,
    pub message: String,
}

impl fmt::Display for FormCalcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FormCalc, line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for FormCalcError {}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Number(String),
    Str(String),
    /// An identifier, or a `$`-accessor such as `$form` (kept as written).
    Name(String),
    /// A keyword, lowercased.
    Keyword(String),
    Op(&'static str),
}

const KEYWORDS: &[&str] = &[
    "and", "or", "not", "eq", "ne", "lt", "le", "gt", "ge", "if", "then", "elseif", "else",
    "endif", "null", "var", "func", "endfunc", "while", "do", "endwhile", "for", "foreach",
    "upto", "downto", "step", "endfor", "in", "break", "continue", "return", "throw",
];

/// Operators, longest first so `<=` is not read as `<` then `=`.
const OPERATORS: &[&str] = &[
    "==", "<>", "<=", ">=", "..", "|", "&", "<", ">", "+", "-", "*", "/", "=", "(", ")", "[",
    "]", ".", ",",
];

struct Lexer<'a> {
    chars: Vec<char>,
    pos: usize,
    line: usize,
    _source: &'a str,
}

fn error(line: usize, message: impl Into<String>) -> FormCalcError {
    FormCalcError {
        line,
        message: message.into(),
    }
}

impl<'a> Lexer<'a> {
    fn tokens(source: &'a str) -> Result<Vec<(Tok, usize)>, FormCalcError> {
        let mut lexer = Lexer {
            chars: source.chars().collect(),
            pos: 0,
            line: 1,
            _source: source,
        };
        let mut out = Vec::new();
        while let Some(tok) = lexer.next()? {
            out.push((tok, lexer.line));
        }
        Ok(out)
    }

    fn peek(&self, ahead: usize) -> Option<char> {
        self.chars.get(self.pos + ahead).copied()
    }

    fn next(&mut self) -> Result<Option<Tok>, FormCalcError> {
        // White space, line terminators and both comment forms.
        loop {
            match self.peek(0) {
                Some('\n') => {
                    self.line += 1;
                    self.pos += 1;
                }
                Some(c) if c.is_whitespace() => self.pos += 1,
                Some(';') => self.skip_line(),
                Some('/') if self.peek(1) == Some('/') => self.skip_line(),
                _ => break,
            }
        }
        let Some(c) = self.peek(0) else {
            return Ok(None);
        };
        if c == '"' {
            return self.string().map(Some);
        }
        if c.is_ascii_digit() || (c == '.' && self.peek(1).is_some_and(|d| d.is_ascii_digit())) {
            return Ok(Some(self.number()));
        }
        if c.is_alphabetic() || c == '_' || c == '$' || c == '!' {
            let start = self.pos;
            self.pos += 1;
            while self
                .peek(0)
                .is_some_and(|d| d.is_alphanumeric() || d == '_' || d == '$')
            {
                self.pos += 1;
            }
            let word: String = self.chars[start..self.pos].iter().collect();
            let lower = word.to_lowercase();
            return Ok(Some(if KEYWORDS.contains(&lower.as_str()) {
                Tok::Keyword(lower)
            } else {
                Tok::Name(word)
            }));
        }
        for op in OPERATORS {
            if op.chars().enumerate().all(|(i, o)| self.peek(i) == Some(o)) {
                self.pos += op.chars().count();
                return Ok(Some(Tok::Op(op)));
            }
        }
        Err(error(self.line, format!("unexpected character {c:?}")))
    }

    fn skip_line(&mut self) {
        while self.peek(0).is_some_and(|c| c != '\n') {
            self.pos += 1;
        }
    }

    /// A string literal: `""` is a quote, `\uXXXX` a code point.
    fn string(&mut self) -> Result<Tok, FormCalcError> {
        let line = self.line;
        self.pos += 1;
        let mut text = String::new();
        loop {
            match self.peek(0) {
                None => return Err(error(line, "an unterminated string")),
                Some('"') if self.peek(1) == Some('"') => {
                    text.push('"');
                    self.pos += 2;
                }
                Some('"') => {
                    self.pos += 1;
                    return Ok(Tok::Str(text));
                }
                Some('\\') if self.peek(1) == Some('u') => {
                    let hex: String = (2..6).filter_map(|i| self.peek(i)).collect();
                    let code = u32::from_str_radix(&hex, 16)
                        .ok()
                        .and_then(char::from_u32)
                        .ok_or_else(|| error(self.line, format!("\\u{hex} is not a character")))?;
                    text.push(code);
                    self.pos += 6;
                }
                Some(c) => {
                    if c == '\n' {
                        self.line += 1;
                    }
                    text.push(c);
                    self.pos += 1;
                }
            }
        }
    }

    fn number(&mut self) -> Tok {
        let start = self.pos;
        while self.peek(0).is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.peek(0) == Some('.') && self.peek(1).is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
            while self.peek(0).is_some_and(|c| c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(0), Some('e' | 'E')) {
            let sign = usize::from(matches!(self.peek(1), Some('+' | '-')));
            if self.peek(1 + sign).is_some_and(|c| c.is_ascii_digit()) {
                self.pos += 1 + sign;
                while self.peek(0).is_some_and(|c| c.is_ascii_digit()) {
                    self.pos += 1;
                }
            }
        }
        Tok::Number(self.chars[start..self.pos].iter().collect())
    }
}

/// An accessor: the node or property a name chain refers to.
struct Accessor {
    /// JavaScript for everything but the last step.
    base: Option<String>,
    /// The last step: a property name, or a whole expression when the chain
    /// is a single `$` accessor.
    last: Step,
}

enum Step {
    /// The whole chain is this JavaScript expression (`this`, `xfa.form`).
    Whole(String),
    /// `.name` or `.name[n]` on the base; a bare first name when the base
    /// is `None`.
    Name { name: String, index: Option<String> },
}

impl Accessor {
    /// JavaScript reading the node or property.
    fn read(&self) -> String {
        match (&self.base, &self.last) {
            (_, Step::Whole(js)) => js.clone(),
            (None, Step::Name { name, index: None }) => name.clone(),
            (None, Step::Name { name, index: Some(i) }) => {
                format!("_fc_first_(this, {}, {i})", js_string(name))
            }
            (Some(base), Step::Name { name, index: None }) => {
                format!("_fc_get_({base}, {})", js_string(name))
            }
            (Some(base), Step::Name { name, index: Some(i) }) => {
                format!("_fc_index_({base}, {}, {i})", js_string(name))
            }
        }
    }

    /// JavaScript assigning the value `value` (already a FormCalc value).
    fn assign(&self, value: &str) -> String {
        match (&self.base, &self.last) {
            (Some(base), Step::Name { name, index: None }) => {
                format!("_fc_set_({base}, {}, {value})", js_string(name))
            }
            _ => format!("_fc_set_node_({}, {value})", self.read()),
        }
    }
}

fn js_string(s: &str) -> String {
    serde_json::to_string(s).expect("a string serialises")
}

struct Parser {
    toks: Vec<(Tok, usize)>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|(t, _)| t)
    }

    fn line(&self) -> usize {
        self.toks
            .get(self.pos)
            .or_else(|| self.toks.last())
            .map_or(1, |(_, l)| *l)
    }

    fn bump(&mut self) -> Option<Tok> {
        let tok = self.toks.get(self.pos).map(|(t, _)| t.clone());
        self.pos += 1;
        tok
    }

    fn is_op(&self, op: &str) -> bool {
        matches!(self.peek(), Some(Tok::Op(o)) if *o == op)
    }

    fn is_keyword(&self, k: &str) -> bool {
        matches!(self.peek(), Some(Tok::Keyword(w)) if w == k)
    }

    fn expect_op(&mut self, op: &str) -> Result<(), FormCalcError> {
        if self.is_op(op) {
            self.pos += 1;
            Ok(())
        } else {
            Err(error(self.line(), format!("expected {op:?}")))
        }
    }

    fn expect_keyword(&mut self, k: &str) -> Result<(), FormCalcError> {
        if self.is_keyword(k) {
            self.pos += 1;
            Ok(())
        } else {
            Err(error(self.line(), format!("expected {k:?}")))
        }
    }

    /// Expressions up to (not including) one of `stops` or the end, as
    /// JavaScript statements that leave the last value in `_fc_`.
    fn expression_list(&mut self, stops: &[&str]) -> Result<String, FormCalcError> {
        let mut out = String::new();
        while let Some(tok) = self.peek() {
            if let Tok::Keyword(k) = tok
                && stops.contains(&k.as_str())
            {
                break;
            }
            out.push_str(&self.statement()?);
        }
        Ok(out)
    }

    fn statement(&mut self) -> Result<String, FormCalcError> {
        if let Some(Tok::Keyword(k)) = self.peek() {
            match k.as_str() {
                "if" => return self.if_expression(),
                "var" | "func" | "while" | "for" | "foreach" | "break" | "continue"
                | "return" | "throw" => {
                    return Err(error(self.line(), format!("`{k}` is not supported")));
                }
                _ => {}
            }
        }
        // An assignment is an accessor followed by `=`.
        let start = self.pos;
        if let Ok(target) = self.accessor()
            && self.is_op("=")
        {
            self.pos += 1;
            let value = self.expression()?;
            return Ok(format!("_fc_ = {};\n", target.assign(&format!("_fc_v_({value})"))));
        }
        self.pos = start;
        let value = self.expression()?;
        Ok(format!("_fc_ = _fc_v_({value});\n"))
    }

    fn if_expression(&mut self) -> Result<String, FormCalcError> {
        self.expect_keyword("if")?;
        let mut out = String::new();
        let cond = self.condition()?;
        self.expect_keyword("then")?;
        let body = self.expression_list(&["elseif", "else", "endif"])?;
        out.push_str(&format!("if (_fc_b_({cond})) {{\n{body}}}"));
        while self.is_keyword("elseif") {
            self.pos += 1;
            let cond = self.condition()?;
            self.expect_keyword("then")?;
            let body = self.expression_list(&["elseif", "else", "endif"])?;
            out.push_str(&format!(" else if (_fc_b_({cond})) {{\n{body}}}"));
        }
        if self.is_keyword("else") {
            self.pos += 1;
            let body = self.expression_list(&["endif"])?;
            out.push_str(&format!(" else {{\n{body}}}"));
        }
        self.expect_keyword("endif")?;
        out.push('\n');
        Ok(out)
    }

    fn condition(&mut self) -> Result<String, FormCalcError> {
        self.expect_op("(")?;
        let cond = self.expression()?;
        self.expect_op(")")?;
        Ok(cond)
    }

    fn expression(&mut self) -> Result<String, FormCalcError> {
        self.binary(0)
    }

    /// The binary operators by precedence level, lowest first.
    fn binary(&mut self, level: usize) -> Result<String, FormCalcError> {
        const LEVELS: &[&[(&str, &str)]] = &[
            &[("|", "or"), ("or", "or")],
            &[("&", "and"), ("and", "and")],
            &[("==", "=="), ("eq", "=="), ("<>", "<>"), ("ne", "<>")],
            &[
                ("<=", "<="),
                ("le", "<="),
                (">=", ">="),
                ("ge", ">="),
                ("<", "<"),
                ("lt", "<"),
                (">", ">"),
                ("gt", ">"),
            ],
            &[("+", "+"), ("-", "-")],
            &[("*", "*"), ("/", "/")],
        ];
        if level == LEVELS.len() {
            return self.unary();
        }
        let mut left = self.binary(level + 1)?;
        loop {
            let op = LEVELS[level].iter().find_map(|(spelling, op)| match self.peek() {
                Some(Tok::Op(o)) if o == spelling => Some(*op),
                Some(Tok::Keyword(k)) if k == spelling => Some(*op),
                _ => None,
            });
            let Some(op) = op else {
                return Ok(left);
            };
            self.pos += 1;
            let right = self.binary(level + 1)?;
            left = format!("_fc_op_({}, {left}, {right})", js_string(op));
        }
    }

    fn unary(&mut self) -> Result<String, FormCalcError> {
        if self.is_op("-") || self.is_op("+") || self.is_keyword("not") {
            let op = match self.bump() {
                Some(Tok::Op(o)) => o.to_string(),
                _ => "not".to_string(),
            };
            let operand = self.unary()?;
            return Ok(format!("_fc_unary_({}, {operand})", js_string(&op)));
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<String, FormCalcError> {
        let line = self.line();
        match self.peek().cloned() {
            Some(Tok::Number(n)) => {
                self.pos += 1;
                Ok(n)
            }
            Some(Tok::Str(s)) => {
                self.pos += 1;
                Ok(js_string(&s))
            }
            Some(Tok::Keyword(k)) if k == "null" => {
                self.pos += 1;
                Ok("null".to_string())
            }
            Some(Tok::Op("(")) => {
                self.pos += 1;
                let inner = self.expression()?;
                self.expect_op(")")?;
                Ok(format!("({inner})"))
            }
            Some(Tok::Name(_)) => Ok(self.accessor()?.read()),
            Some(Tok::Keyword(k)) => Err(error(line, format!("`{k}` is not supported here"))),
            Some(Tok::Op(o)) => Err(error(line, format!("unexpected {o:?}"))),
            None => Err(error(line, "the script ends in the middle of an expression")),
        }
    }

    /// `[n]` after a name, when present.
    fn index(&mut self) -> Result<Option<String>, FormCalcError> {
        if !self.is_op("[") {
            return Ok(None);
        }
        self.pos += 1;
        let index = match self.bump() {
            Some(Tok::Number(n)) if n.chars().all(|c| c.is_ascii_digit()) => n,
            _ => {
                return Err(error(
                    self.line(),
                    "only a literal index `[n]` is supported (not `[*]`, `[+n]` or a predicate)",
                ));
            }
        };
        self.expect_op("]")?;
        Ok(Some(index))
    }

    fn accessor(&mut self) -> Result<Accessor, FormCalcError> {
        let line = self.line();
        let Some(Tok::Name(first)) = self.bump() else {
            return Err(error(line, "expected a name"));
        };
        let mut acc = match first.as_str() {
            "$" => Accessor {
                base: None,
                last: Step::Whole("this".to_string()),
            },
            "$form" | "$layout" | "$host" | "$event" | "$xfa" => Accessor {
                base: None,
                last: Step::Whole(match first.as_str() {
                    "$form" => "xfa.form".to_string(),
                    "$xfa" => "xfa".to_string(),
                    other => format!("xfa.{}", &other[1..]),
                }),
            },
            name if name.starts_with('$') || name.starts_with('!') => {
                return Err(error(line, format!("`{name}` is not supported")));
            }
            name => Accessor {
                base: None,
                last: Step::Name {
                    name: name.to_string(),
                    index: self.index()?,
                },
            },
        };
        while self.is_op(".") || self.is_op("..") {
            if self.is_op("..") {
                return Err(error(self.line(), "`..` is not supported"));
            }
            self.pos += 1;
            let Some(Tok::Name(name)) = self.bump() else {
                return Err(error(
                    self.line(),
                    "only `.name` is supported after a name (not `.#`, `.*` or a method call)",
                ));
            };
            acc = Accessor {
                base: Some(acc.read()),
                last: Step::Name {
                    name,
                    index: self.index()?,
                },
            };
        }
        if self.is_op("(") {
            return Err(error(self.line(), "function and method calls are not supported"));
        }
        Ok(acc)
    }
}

/// `source`, a FormCalc script, as the body of a JavaScript function that
/// runs it with the script's container as `this` and returns its value
/// (the value of the last expression evaluated).
pub fn transpile(source: &str) -> Result<String, FormCalcError> {
    let mut parser = Parser {
        toks: Lexer::tokens(source)?,
        pos: 0,
    };
    let body = parser.expression_list(&[])?;
    if parser.pos < parser.toks.len() {
        return Err(error(parser.line(), "unexpected text after the script"));
    }
    Ok(format!("var _fc_ = null;\n{body}return _fc_;"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_acav_page_number_assigns_the_containers_value() {
        assert_eq!(
            transpile("// a comment\n$ = $.parent.index + 1").unwrap(),
            "var _fc_ = null;\n_fc_ = _fc_set_node_(this, _fc_v_(_fc_op_(\"+\", _fc_get_(_fc_get_(this, \"parent\"), \"index\"), 1)));\nreturn _fc_;"
        );
    }

    #[test]
    fn an_expression_alone_is_the_scripts_value() {
        assert_eq!(
            transpile("A + B ; trailing comment").unwrap(),
            "var _fc_ = null;\n_fc_ = _fc_v_(_fc_op_(\"+\", A, B));\nreturn _fc_;"
        );
    }

    #[test]
    fn operators_bind_by_the_specs_precedence() {
        let js = transpile("1 + 2 * 3 == 7 and not 0").unwrap();
        assert!(
            js.contains(
                "_fc_op_(\"and\", _fc_op_(\"==\", _fc_op_(\"+\", 1, _fc_op_(\"*\", 2, 3)), 7), _fc_unary_(\"not\", 0))"
            ),
            "{js}"
        );
        assert!(transpile("A lt 2 | B ge 3").unwrap().contains("_fc_op_(\"or\", _fc_op_(\"<\", A, 2), _fc_op_(\">=\", B, 3))"));
    }

    #[test]
    fn if_elseif_else_becomes_a_javascript_if_chain() {
        let js = transpile("if (A > 2) then \"big\" elseif (A == 2) then \"two\" else \"small\" endif").unwrap();
        assert!(js.contains("if (_fc_b_(_fc_op_(\">\", A, 2))) {\n_fc_ = _fc_v_(\"big\");\n}"), "{js}");
        assert!(js.contains(" else if (_fc_b_(_fc_op_(\"==\", A, 2))) {"));
        assert!(js.contains(" else {\n_fc_ = _fc_v_(\"small\");\n}"));
    }

    #[test]
    fn strings_numbers_and_indices_are_read() {
        let js = transpile("Row[1].Amount = \"He said \"\"x\"\"\" ; .5e1").unwrap();
        assert!(js.contains("_fc_set_(_fc_first_(this, \"Row\", 1), \"Amount\", _fc_v_(\"He said \\\"x\\\"\"))"), "{js}");
        assert!(!js.contains(".5e1"), "the rest of the line after ';' is a comment");
        assert!(transpile("x = .5e1").unwrap().contains(".5e1"));
    }

    #[test]
    fn unsupported_constructs_are_refused_with_their_line() {
        let refused = |src: &str| transpile(src).unwrap_err();
        assert_eq!(refused("A = 1\nforeach x in (A) do\nendfor").line, 2);
        assert!(refused("Sum(A[*])").message.contains("function and method calls"));
        assert!(refused("A[*]").message.contains("literal index"));
        assert!(refused("A..B").message.contains("`..`"));
        assert!(refused("$data.A").message.contains("$data"));
        assert!(refused("\"open").message.contains("unterminated"));
    }
}
