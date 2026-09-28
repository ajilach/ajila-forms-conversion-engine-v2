//! Low-level parsing of one `INSERT` line, inverting
//! [`crate::sql::sql_string`]'s own quoting exactly.
//!
//! A strict, line-oriented parser is the right tradeoff here, not a general
//! SQL grammar: [`crate::sql::to_sql`] guarantees every `INSERT` fits on a
//! single physical line (its own module doc says so, and
//! [`sql`](crate::sql)'s own test proves a value containing a real newline
//! still keeps one), and every real, machine-generated dump this crate
//! decodes (the reference converter's own output) keeps that same
//! discipline. A hand-authored dump that violates it is out of scope --
//! `decode` returns a [`super::DecodeError`] naming the line, never a
//! best-effort guess.

use std::collections::BTreeMap;

use super::DecodeError;

/// One parsed `INSERT INTO app_redacto.<table> (<cols>) VALUES (<vals>);`
/// line, with values still in their raw SQL-literal spelling (quotes and
/// escapes intact) -- [`super::unquote`] turns one into a Rust `String`.
pub struct ParsedInsert {
    pub table: String,
    /// Column name -> raw value token, e.g. `"'DRAFT'"` or `e'a\\nb'` or a
    /// bare `1`. A map, not a `Vec`, since a table's own column order in the
    /// source line is not guaranteed to match the order this crate reads
    /// them in.
    pub columns: BTreeMap<String, String>,
}

/// Parses one line if it is an `INSERT INTO app_redacto....` statement;
/// `None` for `BEGIN;`, `COMMIT;`, a comment, a blank line, or an `INSERT`
/// into any other schema (never seen in a Redacto dump, but not this
/// crate's business to reject at this layer -- [`super::decode`] rejects an
/// unrecognised table by name instead, with more context).
pub fn parse_insert_line(line: &str) -> Result<Option<ParsedInsert>, DecodeError> {
    let line = line.trim();
    let Some(rest) = line.strip_prefix("INSERT INTO app_redacto.") else {
        return Ok(None);
    };

    let paren = rest.find('(').ok_or_else(|| {
        DecodeError::Malformed(format!("INSERT with no column list: {line:?}"))
    })?;
    let table = rest[..paren].trim().to_owned();
    let after_table = &rest[paren..];

    let columns_close = after_table.find(')').ok_or_else(|| {
        DecodeError::Malformed(format!("INSERT column list never closes: {line:?}"))
    })?;
    let column_list = &after_table[1..columns_close];
    let column_names: Vec<String> = column_list.split(',').map(|c| c.trim().to_owned()).collect();

    let after_columns = &after_table[columns_close + 1..];
    let values_start = after_columns.find("VALUES (").ok_or_else(|| {
        DecodeError::Malformed(format!("INSERT has no VALUES clause: {line:?}"))
    })?;
    let values_raw = &after_columns[values_start + "VALUES (".len()..];
    let values_raw = values_raw
        .strip_suffix(");")
        .ok_or_else(|| DecodeError::Malformed(format!("INSERT does not end in ');': {line:?}")))?;

    let values = split_top_level_values(values_raw);
    if values.len() != column_names.len() {
        return Err(DecodeError::Malformed(format!(
            "{} columns but {} values in: {line:?}",
            column_names.len(),
            values.len()
        )));
    }

    Ok(Some(ParsedInsert {
        table,
        columns: column_names.into_iter().zip(values).collect(),
    }))
}

/// Splits a `VALUES (...)` body on top-level commas -- ones outside a
/// `'...'`/`e'...'` literal, where `''` is a doubled, non-terminating quote.
/// Each returned token keeps its raw spelling (quotes, `e` prefix, escapes)
/// for [`super::unquote`] to interpret.
fn split_top_level_values(raw: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut current = String::new();
    let mut chars = raw.chars().peekable();
    let mut in_quote = false;

    while let Some(c) = chars.next() {
        if in_quote {
            current.push(c);
            if c == '\'' {
                if chars.peek() == Some(&'\'') {
                    current.push(chars.next().expect("peeked"));
                } else {
                    in_quote = false;
                }
            }
            continue;
        }
        match c {
            '\'' => {
                in_quote = true;
                current.push(c);
            }
            ',' => {
                values.push(current.trim().to_owned());
                current.clear();
            }
            _ => current.push(c),
        }
    }
    if !current.trim().is_empty() {
        values.push(current.trim().to_owned());
    }
    values
}

/// The exact inverse of [`crate::sql::sql_string`].
pub fn unquote(token: &str) -> Result<String, DecodeError> {
    if let Some(body) = token.strip_prefix("e'").and_then(|b| b.strip_suffix('\'')) {
        return unescape(body, token);
    }
    if let Some(body) = token.strip_prefix('\'').and_then(|b| b.strip_suffix('\'')) {
        return Ok(body.replace("''", "'"));
    }
    Err(DecodeError::Malformed(format!("not a quoted string literal: {token:?}")))
}

fn unescape(body: &str, original: &str) -> Result<String, DecodeError> {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                // `sql_string` only ever doubles a literal quote, never
                // backslash-escapes one, even in the `e'...'` form.
                match chars.next() {
                    Some('\'') => out.push('\''),
                    _ => {
                        return Err(DecodeError::Malformed(format!(
                            "unescaped quote in e'...' literal: {original:?}"
                        )));
                    }
                }
            }
            '\\' => match chars.next() {
                Some('\\') => out.push('\\'),
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                other => {
                    return Err(DecodeError::Malformed(format!(
                        "unrecognised escape '\\{other:?}' in {original:?}"
                    )));
                }
            },
            other => out.push(other),
        }
    }
    Ok(out)
}

pub fn parse_i64(token: &str, column: &'static str) -> Result<i64, DecodeError> {
    token
        .trim()
        .parse()
        .map_err(|_| DecodeError::Malformed(format!("column {column}: not an integer: {token:?}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::sql_string;

    #[test]
    fn round_trips_every_escape_the_encoder_can_produce() {
        for original in ["hello", "O'Brien", "a\nb", "a\tb", "a\\b", "a\rb", ""] {
            let quoted = sql_string(original);
            assert_eq!(unquote(&quoted).unwrap(), original, "round trip of {original:?}");
        }
    }

    #[test]
    fn parses_a_real_insert_line() {
        let line = "INSERT INTO app_redacto.assets (id, created, asset_id, asset_type) VALUES ('a-1', '1970-01-01 00:00:00.000', 'b-1', 'TEXT');";
        let parsed = parse_insert_line(line).unwrap().expect("an INSERT line");
        assert_eq!(parsed.table, "assets");
        assert_eq!(unquote(&parsed.columns["id"]).unwrap(), "a-1");
        assert_eq!(unquote(&parsed.columns["asset_type"]).unwrap(), "TEXT");
    }

    #[test]
    fn parses_a_line_whose_value_contains_a_comma_and_parens() {
        let line = "INSERT INTO app_redacto.asset_version (id, created, language, version, status, content, asset_fk_id) VALUES ('a', '1970-01-01 00:00:00.000', 'en', 1, 'DRAFT', '<p>Hello, world (really)</p>', 'a-1');";
        let parsed = parse_insert_line(line).unwrap().expect("an INSERT line");
        assert_eq!(unquote(&parsed.columns["content"]).unwrap(), "<p>Hello, world (really)</p>");
        assert_eq!(parse_i64(&parsed.columns["version"], "version").unwrap(), 1);
    }

    #[test]
    fn non_insert_lines_return_none() {
        assert!(parse_insert_line("BEGIN;").unwrap().is_none());
        assert!(parse_insert_line("COMMIT;").unwrap().is_none());
        assert!(parse_insert_line("-- a comment").unwrap().is_none());
        assert!(parse_insert_line("").unwrap().is_none());
    }
}
