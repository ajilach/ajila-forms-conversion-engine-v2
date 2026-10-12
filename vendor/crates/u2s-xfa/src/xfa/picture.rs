//! Picture clauses (XFA 3.3 Chapter 26, "Picture Clause Specification", and
//! the "Picture Clause Reference"): how a field's canonical value is shown
//! (`<format><picture>`) and how what a person types is read back into it
//! (`<ui>`'s picture).
//!
//! Canonical values are the spec's: a date is `YYYY-MM-DD`, a time
//! `HH:MM:SS`, a number a plain decimal (`-1234.5`), text itself.
//!
//! A picture is parsed once into [`Picture`]; formatting and parsing are pure
//! functions of a value and a [`Locale`]. Formatting tries each alternative
//! (`a|b`) in turn and takes the first the value fits; a value no
//! alternative fits is shown as it is, as the spec says. What is not
//! supported -- time zones, `E` exponents, Asian symbols, the global `?*+`
//! symbols, a clause naming its own locale, `datetime{}` -- is refused when
//! the picture is parsed, so a field never shows a half-formatted value.

use super::locale::Locale;
use rust_decimal::{Decimal, RoundingStrategy};
use std::str::FromStr;

/// A picture clause category (`date{...}`, `num{...}`, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Date,
    Time,
    Num,
    Text,
    /// Matches an empty value.
    Null,
    /// Matches a numeric zero.
    Zero,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Sym {
    /// A run of one symbol letter or character, `count` long.
    Letter(char, usize),
    Lit(String),
}

#[derive(Debug, Clone, PartialEq)]
struct Clause {
    category: Category,
    /// `date.long{}` and the like: the locale's named pattern stands in for
    /// the symbols, so they are only known once the locale is.
    named: Option<String>,
    symbols: Vec<Sym>,
}

#[derive(Debug, Clone, PartialEq)]
enum Segment {
    Literal(String),
    Clause(Clause),
}

/// A parsed picture clause: one or more alternatives, each a sequence of
/// clauses and literals.
#[derive(Debug, Clone, PartialEq)]
pub struct Picture {
    source: String,
    alternatives: Vec<Vec<Segment>>,
}

impl Picture {
    /// Parse `source`. An error names what is malformed or unsupported.
    pub fn parse(source: &str) -> Result<Picture, String> {
        let alternatives = split_unquoted(source, '|')
            .into_iter()
            .map(|alt| parse_alternative(alt).map_err(|e| format!("picture {source:?}: {e}")))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Picture {
            source: source.to_string(),
            alternatives,
        })
    }

    /// The picture as the template wrote it.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The category of every clause, in order of appearance.
    pub fn categories(&self) -> Vec<Category> {
        self.alternatives
            .iter()
            .flatten()
            .filter_map(|s| match s {
                Segment::Clause(c) => Some(c.category),
                Segment::Literal(_) => None,
            })
            .collect()
    }

    /// `raw` (canonical) as the picture shows it, from the first alternative
    /// it fits; `None` when it fits none, in which case it is shown as is.
    pub fn format(&self, raw: &str, locale: &Locale) -> Option<String> {
        self.alternatives.iter().find_map(|alt| {
            alt.iter()
                .map(|segment| match segment {
                    Segment::Literal(text) => Some(text.clone()),
                    Segment::Clause(clause) => format_clause(clause, raw, locale),
                })
                .collect::<Option<String>>()
        })
    }

    /// `input` read through the picture into its canonical value, by the
    /// first alternative it matches; `None` when it matches none.
    pub fn parse_input(&self, input: &str, locale: &Locale) -> Option<String> {
        self.alternatives
            .iter()
            .find_map(|alt| parse_alternative_input(alt, input, locale))
    }
}

/// Whether `raw` is already a canonical value of `category`: a date
/// `YYYY-MM-DD`, a time `HH:MM:SS`, a plain decimal number. Text, null and
/// zero values have no other form, so any value is canonical for them.
pub fn is_canonical(category: Category, raw: &str) -> bool {
    match category {
        Category::Date => Date::parse_canonical(raw).is_some(),
        Category::Time => Time::parse_canonical(raw).is_some(),
        Category::Num => Decimal::from_str(raw.trim()).is_ok(),
        Category::Text | Category::Null | Category::Zero => true,
    }
}

// ── picture parsing ────────────────────────────────────────────────────────

/// `s` split on `sep` outside single quotes.
fn split_unquoted(s: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut start, mut quoted) = (0, false);
    for (i, c) in s.char_indices() {
        if c == '\'' {
            quoted = !quoted;
        } else if c == sep && !quoted {
            parts.push(&s[start..i]);
            start = i + c.len_utf8();
        }
    }
    parts.push(&s[start..]);
    parts
}

/// A quoted literal starting at `chars[i]` (a `'`): its text, with `''` as
/// a quote, and the index after the closing quote.
fn quoted(chars: &[char], i: usize) -> Result<(String, usize), String> {
    let mut text = String::new();
    let mut j = i + 1;
    loop {
        match chars.get(j) {
            None => return Err("an unterminated quoted literal".to_string()),
            Some('\'') if chars.get(j + 1) == Some(&'\'') => {
                text.push('\'');
                j += 2;
            }
            Some('\'') => return Ok((text, j + 1)),
            Some(c) => {
                text.push(*c);
                j += 1;
            }
        }
    }
}

fn parse_alternative(alt: &str) -> Result<Vec<Segment>, String> {
    let chars: Vec<char> = alt.chars().collect();
    let mut segments = Vec::new();
    let mut i = 0;
    let mut saw_clause = false;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            let (text, next) = quoted(&chars, i)?;
            segments.push(Segment::Literal(text));
            i = next;
        } else if c.is_ascii_alphabetic() {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '.') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            if chars.get(i) == Some(&'(') {
                return Err(format!("{word}: a clause naming its own locale is not supported"));
            }
            if chars.get(i) != Some(&'{') {
                // No category: the whole alternative is bare symbols.
                if saw_clause || !segments.is_empty() {
                    return Err(format!("{word:?} is outside any clause"));
                }
                return Ok(vec![Segment::Clause(bare_clause(alt)?)]);
            }
            let close = find_close(&chars, i)?;
            let body: String = chars[i + 1..close].iter().collect();
            segments.push(Segment::Clause(clause(&word, &body)?));
            saw_clause = true;
            i = close + 1;
        } else if c.is_whitespace() {
            segments.push(Segment::Literal(c.to_string()));
            i += 1;
        } else if !saw_clause && segments.is_empty() {
            return Ok(vec![Segment::Clause(bare_clause(alt)?)]);
        } else {
            segments.push(Segment::Literal(c.to_string()));
            i += 1;
        }
    }
    if segments.is_empty() {
        return Ok(vec![Segment::Literal(String::new())]);
    }
    Ok(segments)
}

/// The index of the `}` closing the `{` at `open`, quotes respected.
fn find_close(chars: &[char], open: usize) -> Result<usize, String> {
    let mut j = open + 1;
    while j < chars.len() {
        match chars[j] {
            '\'' => j = quoted(chars, j)?.1,
            '}' => return Ok(j),
            _ => j += 1,
        }
    }
    Err("an unclosed '{'".to_string())
}

fn clause(word: &str, body: &str) -> Result<Clause, String> {
    let (name, sub) = match word.split_once('.') {
        Some((n, s)) => (n, Some(s.to_string())),
        None => (word, None),
    };
    let category = match name {
        "date" => Category::Date,
        "time" => Category::Time,
        "num" => Category::Num,
        "text" => Category::Text,
        "null" => Category::Null,
        "zero" => Category::Zero,
        "datetime" => return Err("datetime{} is not supported".to_string()),
        other => return Err(format!("{other:?} is not a picture category")),
    };
    let named = match (category, sub, body.trim().is_empty()) {
        (Category::Date | Category::Time, Some(sub), true) => match sub.as_str() {
            "full" | "long" | "med" | "short" => Some(sub),
            other => return Err(format!("{name}.{other} is not a named pattern")),
        },
        (Category::Date | Category::Time, None, true) => Some("med".to_string()),
        (_, Some(sub), _) => return Err(format!("{name}.{sub} is not supported")),
        _ => None,
    };
    let symbols = if named.is_some() {
        Vec::new()
    } else {
        tokenize(category, body)?
    };
    Ok(Clause {
        category,
        named,
        symbols,
    })
}

/// A picture with no category name: the category is inferred from its
/// symbols, and an ambiguous picture is an error, as the spec says.
fn bare_clause(body: &str) -> Result<Clause, String> {
    let unquoted: String = split_unquoted(body, '\'')
        .iter()
        .step_by(2)
        .copied()
        .collect();
    let has = |set: &str| unquoted.chars().any(|c| set.contains(c));
    let category = if has("DYJEGw") {
        Category::Date
    } else if has("hHkKSAF") {
        Category::Time
    } else if has("zZ8vV$%") || (has("9") && has(".,")) {
        Category::Num
    } else if has("AXO") {
        Category::Text
    } else {
        return Err(format!("{body:?} names no category and its symbols do not tell one"));
    };
    clause(
        match category {
            Category::Date => "date",
            Category::Time => "time",
            Category::Num => "num",
            _ => "text",
        },
        body,
    )
}

/// The symbols of `body` in `category`: quoted text and characters that are
/// not symbols of the category are literals.
fn tokenize(category: Category, body: &str) -> Result<Vec<Sym>, String> {
    let symbol_chars: &str = match category {
        Category::Date => "DJMEeGYwW",
        Category::Time => "hkHKMSFAZz",
        Category::Num => "98zZ.vV,sS()$%E",
        Category::Text => "AXO09",
        Category::Null | Category::Zero => "",
    };
    let chars: Vec<char> = body.chars().collect();
    let mut syms: Vec<Sym> = Vec::new();
    let push_lit = |syms: &mut Vec<Sym>, text: String| match syms.last_mut() {
        Some(Sym::Lit(prev)) => prev.push_str(&text),
        _ => syms.push(Sym::Lit(text)),
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            let (text, next) = quoted(&chars, i)?;
            push_lit(&mut syms, text);
            i = next;
            continue;
        }
        if category == Category::Num
            && let Some(pair) = chars.get(i..i + 2)
            && matches!(pair, ['C', 'R'] | ['c', 'r'] | ['D', 'B'] | ['d', 'b'])
        {
            syms.push(Sym::Letter(pair[0], 2));
            i += 2;
            continue;
        }
        if symbol_chars.contains(c) {
            // Digit symbols are one position each; letters are runs.
            let run = if category == Category::Num || category == Category::Text {
                if c == '$' && chars.get(i + 1) == Some(&'$') { 2 } else { 1 }
            } else {
                chars[i..].iter().take_while(|&&x| x == c).count()
            };
            syms.push(Sym::Letter(c, run));
            i += run;
            continue;
        }
        if c.is_ascii_alphabetic() || matches!(c, '?' | '*' | '+') && category != Category::Num {
            return Err(format!("{c:?} is not a {category:?} picture symbol"));
        }
        push_lit(&mut syms, c.to_string());
        i += 1;
    }
    validate(category, &syms)?;
    Ok(syms)
}

fn validate(category: Category, syms: &[Sym]) -> Result<(), String> {
    for sym in syms {
        let Sym::Letter(c, n) = sym else { continue };
        let ok = match (category, c) {
            (Category::Date, 'D') => matches!(n, 1 | 2),
            (Category::Date, 'J') => matches!(n, 1 | 3),
            (Category::Date, 'M') => matches!(n, 1..=4),
            (Category::Date, 'E') => matches!(n, 1 | 3 | 4),
            (Category::Date, 'e' | 'G' | 'w') => *n == 1,
            (Category::Date, 'Y') => matches!(n, 2 | 4),
            (Category::Date, 'W') => *n == 2,
            (Category::Time, 'h' | 'k' | 'H' | 'K' | 'M' | 'S') => matches!(n, 1 | 2),
            (Category::Time, 'F') => *n == 3,
            (Category::Time, 'A') => *n == 1,
            (Category::Time, 'Z' | 'z') => {
                return Err("time zone symbols are not supported".to_string());
            }
            (Category::Num, 'E') => {
                return Err("the exponent symbol E is not supported".to_string());
            }
            (Category::Num | Category::Text, _) => true,
            _ => false,
        };
        if !ok {
            return Err(format!("{} is not a {category:?} picture symbol", c.to_string().repeat(*n)));
        }
    }
    Ok(())
}

/// The symbols a clause formats with: its own, or its locale's named
/// pattern.
fn clause_symbols(clause: &Clause, locale: &Locale) -> Option<Vec<Sym>> {
    match &clause.named {
        None => Some(clause.symbols.clone()),
        Some(name) => {
            let pattern = match clause.category {
                Category::Date => locale.date_pattern(name),
                _ => locale.time_pattern(name),
            }?;
            tokenize(clause.category, pattern).ok()
        }
    }
}

fn literal_text(syms: &[Sym]) -> String {
    syms.iter()
        .map(|s| match s {
            Sym::Lit(t) => t.clone(),
            Sym::Letter(c, n) => c.to_string().repeat(*n),
        })
        .collect()
}

// ── calendar ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Date {
    year: i32,
    month: u32,
    day: u32,
}

fn is_leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

impl Date {
    fn new(year: i32, month: u32, day: u32) -> Option<Date> {
        ((1..=12).contains(&month) && day >= 1 && day <= days_in_month(year, month))
            .then_some(Date { year, month, day })
    }

    /// The canonical form, `YYYY-MM-DD`, or a date-time's date part. The
    /// compact `YYYYMMDD` is accepted too.
    fn parse_canonical(raw: &str) -> Option<Date> {
        let raw = raw.trim();
        let date = raw.split('T').next()?;
        let digits: String = date.chars().filter(|c| *c != '-').collect();
        if digits.len() != 8 || !digits.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        if date.contains('-') && date.len() != 10 {
            return None;
        }
        Date::new(
            digits[0..4].parse().ok()?,
            digits[4..6].parse().ok()?,
            digits[6..8].parse().ok()?,
        )
    }

    fn canonical(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// Days since 1970-01-01 (H. Hinnant's days-from-civil).
    fn days(self) -> i64 {
        let y = i64::from(self.year) - i64::from(self.month <= 2);
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let m = i64::from(self.month);
        let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(self.day) - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    /// 0 for Sunday.
    fn weekday(self) -> u32 {
        (self.days() + 4).rem_euclid(7) as u32
    }

    fn day_of_year(self) -> u32 {
        (1..self.month).map(|m| days_in_month(self.year, m)).sum::<u32>() + self.day
    }

    /// Week of the month: week 1 is the first run of four or more days of
    /// the month that ends on a Saturday; days before it are week 0.
    fn week_of_month(self) -> u32 {
        let first = Date { day: 1, ..self };
        let first_saturday = 1 + (6 + 7 - first.weekday()) % 7;
        let index = (self.day + 7 - first_saturday - 1) / 7;
        if first_saturday >= 4 { index + 1 } else { index }
    }

    /// ISO-8601 week of the year: week 1 contains January 4.
    fn iso_week(self) -> u32 {
        let iso_weekday = (self.weekday() + 6) % 7; // Monday = 0
        let thursday = self.days() - i64::from(iso_weekday) + 3;
        let year_of_thursday = {
            let mut y = self.year + 1;
            while Date::new(y, 1, 1).map(|d| d.days()).unwrap_or(i64::MAX) > thursday {
                y -= 1;
            }
            y
        };
        let jan1 = Date::new(year_of_thursday, 1, 1).expect("January 1 exists").days();
        ((thursday - jan1) / 7 + 1) as u32
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Time {
    hour: u32,
    minute: u32,
    second: u32,
    millis: u32,
}

impl Time {
    /// `HH:MM:SS[.FFF]`, `HHMMSS`, or a date-time's time part.
    fn parse_canonical(raw: &str) -> Option<Time> {
        let raw = raw.trim();
        let time = raw.split_once('T').map_or(raw, |(_, t)| t);
        let (main, frac) = time.split_once('.').unwrap_or((time, ""));
        let digits: String = main.chars().filter(|c| *c != ':').collect();
        if !(digits.len() == 4 || digits.len() == 6) || !digits.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let t = Time {
            hour: digits[0..2].parse().ok()?,
            minute: digits[2..4].parse().ok()?,
            second: if digits.len() == 6 { digits[4..6].parse().ok()? } else { 0 },
            millis: if frac.is_empty() {
                0
            } else {
                format!("{frac:0<3}")[..3].parse().ok()?
            },
        };
        (t.hour < 24 && t.minute < 60 && t.second < 60).then_some(t)
    }

    fn canonical(self) -> String {
        format!("{:02}:{:02}:{:02}", self.hour, self.minute, self.second)
    }
}

// ── formatting ─────────────────────────────────────────────────────────────

fn format_clause(clause: &Clause, raw: &str, locale: &Locale) -> Option<String> {
    let syms = clause_symbols(clause, locale)?;
    match clause.category {
        Category::Date => format_date(&syms, Date::parse_canonical(raw)?, locale),
        Category::Time => format_time(&syms, Time::parse_canonical(raw)?, locale),
        Category::Num => format_num(&syms, Decimal::from_str(raw.trim()).ok()?, locale),
        Category::Text => format_text(&syms, raw),
        Category::Null => raw.trim().is_empty().then(|| literal_text(&syms)),
        Category::Zero => Decimal::from_str(raw.trim())
            .ok()
            .filter(Decimal::is_zero)
            .map(|_| literal_text(&syms)),
    }
}

fn format_date(syms: &[Sym], date: Date, locale: &Locale) -> Option<String> {
    let mut out = String::new();
    for sym in syms {
        let text = match sym {
            Sym::Lit(t) => t.clone(),
            Sym::Letter('D', 1) => date.day.to_string(),
            Sym::Letter('D', _) => format!("{:02}", date.day),
            Sym::Letter('J', 1) => date.day_of_year().to_string(),
            Sym::Letter('J', _) => format!("{:03}", date.day_of_year()),
            Sym::Letter('M', 1) => date.month.to_string(),
            Sym::Letter('M', 2) => format!("{:02}", date.month),
            Sym::Letter('M', 3) => locale.month_abbrs[date.month as usize - 1].clone(),
            Sym::Letter('M', _) => locale.month_names[date.month as usize - 1].clone(),
            Sym::Letter('E', 1) => (date.weekday() + 1).to_string(),
            Sym::Letter('E', 3) => locale.day_abbrs[date.weekday() as usize].clone(),
            Sym::Letter('E', _) => locale.day_names[date.weekday() as usize].clone(),
            Sym::Letter('e', _) => ((date.weekday() + 6) % 7 + 1).to_string(),
            Sym::Letter('G', _) => locale.eras[usize::from(date.year > 0)].clone(),
            Sym::Letter('Y', 2) => format!("{:02}", date.year.rem_euclid(100)),
            Sym::Letter('Y', _) => format!("{:04}", date.year),
            Sym::Letter('w', _) => date.week_of_month().to_string(),
            Sym::Letter('W', _) => format!("{:02}", date.iso_week()),
            Sym::Letter(..) => return None,
        };
        out.push_str(&text);
    }
    Some(out)
}

fn format_time(syms: &[Sym], time: Time, locale: &Locale) -> Option<String> {
    let hour12 = match time.hour % 12 {
        0 => 12,
        h => h,
    };
    let mut out = String::new();
    for sym in syms {
        let (value, pad) = match sym {
            Sym::Lit(t) => {
                out.push_str(t);
                continue;
            }
            Sym::Letter('A', _) => {
                out.push_str(&locale.meridiems[usize::from(time.hour >= 12)]);
                continue;
            }
            Sym::Letter('F', _) => (time.millis, 3),
            Sym::Letter(c, n) => {
                let value = match c {
                    'h' => hour12,
                    'k' => time.hour % 12,
                    'H' => time.hour,
                    'K' => {
                        if time.hour == 0 {
                            24
                        } else {
                            time.hour
                        }
                    }
                    'M' => time.minute,
                    'S' => time.second,
                    _ => return None,
                };
                (value, *n)
            }
        };
        out.push_str(&format!("{value:0pad$}"));
    }
    Some(out)
}

/// A numeric picture's digit position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DigitKind {
    /// `9`: always a digit, zero when there is none.
    Nine,
    /// `z`: a digit, or nothing for a leading zero.
    LowerZ,
    /// `Z`: a digit, or a space for a leading zero.
    UpperZ,
    /// `8`: a digit when the value has one there.
    Eight,
}

fn digit_kind(c: char) -> Option<DigitKind> {
    match c {
        '9' => Some(DigitKind::Nine),
        'z' => Some(DigitKind::LowerZ),
        'Z' => Some(DigitKind::UpperZ),
        '8' => Some(DigitKind::Eight),
        _ => None,
    }
}

fn is_radix(sym: &Sym) -> bool {
    matches!(sym, Sym::Letter('.' | 'v' | 'V', _))
}

fn has_sign_symbol(syms: &[Sym]) -> bool {
    syms.iter().any(|s| {
        matches!(s, Sym::Letter('s' | 'S' | '(' | ')' | 'C' | 'c' | 'D' | 'd', _))
    })
}

fn format_num(syms: &[Sym], value: Decimal, locale: &Locale) -> Option<String> {
    let negative = value.is_sign_negative() && !value.is_zero();
    if negative && !has_sign_symbol(syms) {
        return None;
    }
    let percent = syms.iter().any(|s| matches!(s, Sym::Letter('%', _)));
    let magnitude = if percent { value.abs() * Decimal::ONE_HUNDRED } else { value.abs() };

    let radix_at = syms.iter().position(is_radix);
    let (int_syms, frac_syms) = match radix_at {
        Some(i) => (&syms[..i], &syms[i + 1..]),
        None => (syms, &[][..]),
    };
    let frac_kinds: Vec<DigitKind> = frac_syms
        .iter()
        .filter_map(|s| match s {
            Sym::Letter(c, _) => digit_kind(*c),
            Sym::Lit(_) => None,
        })
        .collect();
    let has_eight = frac_kinds.contains(&DigitKind::Eight);

    // The fraction digits to show, and whether the radix is shown.
    let given_scale = magnitude.scale() as usize;
    let (rounded, frac_digits, show_radix) = if has_eight {
        let digits = fraction_digits(magnitude, given_scale);
        let show = given_scale > 0;
        (magnitude, digits, show)
    } else {
        let n = frac_kinds.len();
        let rounded = magnitude.round_dp_with_strategy(n as u32, RoundingStrategy::MidpointAwayFromZero);
        let all = fraction_digits(rounded, n);
        let required = frac_kinds.iter().filter(|k| **k == DigitKind::Nine).count();
        let keep = given_scale.min(n).max(required);
        let digits: String = all.chars().take(keep).collect();
        let explicit_radix = matches!(radix_at.map(|i| &syms[i]), Some(Sym::Letter('.' | 'V', _)));
        let show = explicit_radix
            && (required > 0
                || given_scale > 0
                || frac_kinds.contains(&DigitKind::UpperZ)
                || frac_kinds.is_empty() && radix_at.is_some());
        (rounded, digits, show)
    };

    // Integer digits, right-aligned to the picture's integer positions.
    let int_positions = int_syms
        .iter()
        .filter(|s| matches!(s, Sym::Letter(c, _) if digit_kind(*c).is_some()))
        .count();
    let int_value = rounded.trunc().abs();
    let int_str = if int_value.is_zero() {
        String::new()
    } else {
        int_value.to_string()
    };
    if int_str.len() > int_positions {
        return None;
    }
    let mut padded: Vec<Option<char>> = vec![None; int_positions - int_str.len()];
    padded.extend(int_str.chars().map(Some));

    let mut out = String::new();
    let mut position = 0;
    let mut leading = true;
    let mut last_kind = DigitKind::Nine;
    let sign_text = |c: char, n: usize| -> String {
        match (c, n) {
            ('s', _) => if negative { locale.minus.clone() } else { String::new() },
            ('S', _) => if negative { locale.minus.clone() } else { " ".to_string() },
            ('(', _) => if negative { "(".to_string() } else { " ".to_string() },
            (')', _) => if negative { ")".to_string() } else { " ".to_string() },
            ('C', _) => if negative { "CR".to_string() } else { "  ".to_string() },
            ('c', _) => if negative { "CR".to_string() } else { String::new() },
            ('D', _) => if negative { "DB".to_string() } else { "  ".to_string() },
            ('d', _) => if negative { "DB".to_string() } else { String::new() },
            ('$', 2) => locale.currency_iso.clone(),
            ('$', _) => locale.currency_symbol.clone(),
            ('%', _) => locale.percent.clone(),
            _ => String::new(),
        }
    };
    for sym in int_syms {
        match sym {
            Sym::Lit(t) => out.push_str(t),
            Sym::Letter(c, n) => match (digit_kind(*c), c) {
                (Some(kind), _) => {
                    let digit = padded[position];
                    position += 1;
                    last_kind = kind;
                    match (digit, kind) {
                        (Some(d), _) => {
                            out.push(d);
                            leading = false;
                        }
                        (None, DigitKind::Nine) => {
                            out.push('0');
                            leading = false;
                        }
                        (None, DigitKind::UpperZ) => out.push(' '),
                        (None, _) => {}
                    }
                }
                (None, ',') => {
                    if !leading {
                        out.push_str(&locale.grouping);
                    } else if last_kind == DigitKind::UpperZ {
                        out.push(' ');
                    }
                }
                (None, c) => out.push_str(&sign_text(*c, *n)),
            },
        }
    }
    if let Some(i) = radix_at {
        if show_radix && matches!(syms[i], Sym::Letter('.' | 'V', _)) {
            out.push_str(&locale.decimal);
        }
        let mut digits = frac_digits.chars();
        for sym in frac_syms {
            match sym {
                Sym::Lit(t) => out.push_str(t),
                Sym::Letter(c, n) => match digit_kind(*c) {
                    Some(DigitKind::Eight) => out.extend(digits.by_ref()),
                    Some(_) => out.extend(digits.next()),
                    None => out.push_str(&sign_text(*c, *n)),
                },
            }
        }
    }
    Some(out)
}

/// The first `n` fraction digits of `value`, zero-padded.
fn fraction_digits(value: Decimal, n: usize) -> String {
    let text = value.abs().to_string();
    let frac = text.split_once('.').map_or("", |(_, f)| f);
    format!("{frac:0<n$}").chars().take(n).collect()
}

fn format_text(syms: &[Sym], raw: &str) -> Option<String> {
    let mut chars = raw.chars();
    let mut out = String::new();
    for sym in syms {
        match sym {
            Sym::Lit(t) => out.push_str(t),
            Sym::Letter(c, _) => {
                let next = chars.next()?;
                if !text_symbol_accepts(*c, next) {
                    return None;
                }
                out.push(next);
            }
        }
    }
    chars.next().is_none().then_some(out)
}

fn text_symbol_accepts(symbol: char, c: char) -> bool {
    match symbol {
        'A' => c.is_alphabetic(),
        '9' => c.is_ascii_digit(),
        'O' | '0' => c.is_alphanumeric(),
        _ => true,
    }
}

// ── input parsing ──────────────────────────────────────────────────────────

fn parse_alternative_input(alt: &[Segment], input: &str, locale: &Locale) -> Option<String> {
    // A compound picture is read by its first clause; its literals must
    // surround the input as written.
    let mut rest = input;
    let mut clause = None;
    let mut trailing = String::new();
    for segment in alt {
        match segment {
            Segment::Literal(t) if clause.is_none() => rest = rest.strip_prefix(t.as_str())?,
            Segment::Literal(t) => trailing.push_str(t),
            Segment::Clause(c) if clause.is_none() => clause = Some(c),
            Segment::Clause(_) => return None,
        }
    }
    let rest = rest.strip_suffix(trailing.as_str())?;
    let clause = clause?;
    let syms = clause_symbols(clause, locale)?;
    match clause.category {
        Category::Date => parse_date(&syms, rest, locale).map(Date::canonical),
        Category::Time => parse_time(&syms, rest, locale).map(Time::canonical),
        Category::Num => parse_num(&syms, rest, locale),
        Category::Text => parse_text(&syms, rest),
        Category::Null => (rest.trim().is_empty() || rest == literal_text(&syms)).then(String::new),
        Category::Zero => (rest == literal_text(&syms)).then(|| "0".to_string()),
    }
}

/// Up to `max` (at least `min`) ASCII digits from the front of `s`.
fn take_digits(s: &str, min: usize, max: usize) -> Option<(u32, &str)> {
    let n = s.chars().take(max).take_while(char::is_ascii_digit).count();
    (n >= min).then(|| (s[..n].parse().ok(), &s[n..])).and_then(|(v, r)| Some((v?, r)))
}

/// The longest of `names` that `s` starts with (ignoring case): its index
/// and the rest of `s`.
fn take_name<'a>(s: &'a str, names: &[String]) -> Option<(usize, &'a str)> {
    names
        .iter()
        .enumerate()
        .filter(|(_, n)| !n.is_empty())
        .filter(|(_, n)| {
            s.get(..n.len())
                .is_some_and(|head| head.to_lowercase() == n.to_lowercase())
        })
        .max_by_key(|(_, n)| n.len())
        .map(|(i, n)| (i, &s[n.len()..]))
}

fn parse_date(syms: &[Sym], input: &str, locale: &Locale) -> Option<Date> {
    let (mut year, mut month, mut day) = (None, None, None);
    let mut rest = input.trim();
    for sym in syms {
        rest = match sym {
            Sym::Lit(t) => rest.strip_prefix(t.as_str())?,
            Sym::Letter('D', n) => {
                let (v, r) = take_digits(rest, *n, 2)?;
                day = Some(v);
                r
            }
            Sym::Letter('M', n @ (1 | 2)) => {
                let (v, r) = take_digits(rest, *n, 2)?;
                month = Some(v);
                r
            }
            Sym::Letter('M', 3) => {
                let (i, r) = take_name(rest, &locale.month_abbrs)?;
                month = Some(i as u32 + 1);
                r
            }
            Sym::Letter('M', _) => {
                let (i, r) = take_name(rest, &locale.month_names)?;
                month = Some(i as u32 + 1);
                r
            }
            Sym::Letter('Y', 2) => {
                let (v, r) = take_digits(rest, 2, 2)?;
                year = Some(if v < 30 { 2000 + v as i32 } else { 1900 + v as i32 });
                r
            }
            Sym::Letter('Y', _) => {
                let (v, r) = take_digits(rest, 4, 4)?;
                year = Some(v as i32);
                r
            }
            Sym::Letter('E', 3) => take_name(rest, &locale.day_abbrs)?.1,
            Sym::Letter('E', 4) => take_name(rest, &locale.day_names)?.1,
            Sym::Letter('G', _) => take_name(rest, &locale.eras)?.1,
            Sym::Letter('J', n) => take_digits(rest, *n, 3)?.1,
            Sym::Letter('W', _) => take_digits(rest, 2, 2)?.1,
            Sym::Letter(_, _) => take_digits(rest, 1, 1)?.1,
        };
    }
    if !rest.is_empty() {
        return None;
    }
    Date::new(year?, month?, day?)
}

fn parse_time(syms: &[Sym], input: &str, locale: &Locale) -> Option<Time> {
    let mut time = Time { hour: 0, minute: 0, second: 0, millis: 0 };
    let mut pm = None;
    let mut twelve_hour = false;
    let mut rest = input.trim();
    for sym in syms {
        rest = match sym {
            Sym::Lit(t) => rest.strip_prefix(t.as_str())?,
            Sym::Letter('A', _) => {
                let (i, r) = take_name(rest, &locale.meridiems)?;
                pm = Some(i == 1);
                r
            }
            Sym::Letter('F', _) => {
                let (v, r) = take_digits(rest, 3, 3)?;
                time.millis = v;
                r
            }
            Sym::Letter(c, n) => {
                let (v, r) = take_digits(rest, *n, 2)?;
                match c {
                    'h' => {
                        twelve_hour = true;
                        time.hour = v % 12;
                    }
                    'k' => {
                        twelve_hour = true;
                        time.hour = v;
                    }
                    'H' => time.hour = v,
                    'K' => time.hour = v % 24,
                    'M' => time.minute = v,
                    'S' => time.second = v,
                    _ => return None,
                }
                r
            }
        };
    }
    if !rest.is_empty() {
        return None;
    }
    if twelve_hour && pm == Some(true) {
        time.hour += 12;
    }
    (time.hour < 24 && time.minute < 60 && time.second < 60).then_some(time)
}

fn parse_num(syms: &[Sym], input: &str, locale: &Locale) -> Option<String> {
    let mut s = input.trim().to_string();
    let mut negative = false;
    for marker in ["CR", "DB", "cr", "db"] {
        if let Some(stripped) = s.strip_suffix(marker) {
            negative = true;
            s = stripped.trim_end().to_string();
        }
    }
    if s.starts_with('(') && s.ends_with(')') {
        negative = true;
        s = s[1..s.len() - 1].to_string();
    }
    if let Some(stripped) = s.strip_prefix(locale.minus.as_str()) {
        negative = true;
        s = stripped.to_string();
    } else if let Some(stripped) = s.strip_prefix('+') {
        s = stripped.to_string();
    }
    let percent = syms.iter().any(|x| matches!(x, Sym::Letter('%', _)));
    for symbol in [&locale.currency_iso, &locale.currency_symbol, &locale.percent] {
        if !symbol.is_empty() {
            s = s.replace(symbol.as_str(), "");
        }
    }
    // Literals of the picture (a space between digit groups) and the
    // grouping separator carry no value.
    for sym in syms {
        if let Sym::Lit(t) = sym {
            s = s.replace(t.as_str(), "");
        }
    }
    if !locale.grouping.is_empty() {
        s = s.replace(locale.grouping.as_str(), "");
    }
    s = s.replace(' ', "");
    let radix_at = syms.iter().position(is_radix);
    let implied = matches!(radix_at.map(|i| &syms[i]), Some(Sym::Letter('v' | 'V', _)));
    let s = s.replace(locale.decimal.as_str(), ".");
    let digit_count = |part: &[Sym]| {
        part.iter()
            .filter(|x| matches!(x, Sym::Letter(c, _) if digit_kind(*c).is_some()))
            .count()
    };
    let (int_positions, frac_positions) = match radix_at {
        Some(i) => (digit_count(&syms[..i]), digit_count(&syms[i + 1..])),
        None => (digit_count(syms), 0),
    };
    let s = if implied && !s.contains('.') && frac_positions > 0 && s.len() > frac_positions {
        format!("{}.{}", &s[..s.len() - frac_positions], &s[s.len() - frac_positions..])
    } else {
        s
    };
    let (int_part, frac_part) = s.split_once('.').unwrap_or((&s, ""));
    if int_part.is_empty() && frac_part.is_empty()
        || !int_part.chars().chain(frac_part.chars()).all(|c| c.is_ascii_digit())
    {
        return None;
    }
    let has_eight = syms.iter().any(|x| matches!(x, Sym::Letter('8', _)));
    let int_digits = int_part.trim_start_matches('0').len();
    if int_digits > int_positions || (!has_eight && frac_part.len() > frac_positions) {
        return None;
    }
    let mut value = Decimal::from_str(&format!(
        "{}.{}",
        if int_part.is_empty() { "0" } else { int_part },
        if frac_part.is_empty() { "0" } else { frac_part }
    ))
    .ok()?;
    if percent {
        value /= Decimal::ONE_HUNDRED;
    }
    if negative {
        value = -value;
    }
    Some(value.normalize().to_string())
}

fn parse_text(syms: &[Sym], input: &str) -> Option<String> {
    let mut rest = input;
    let mut out = String::new();
    for sym in syms {
        match sym {
            Sym::Lit(t) => rest = rest.strip_prefix(t.as_str())?,
            Sym::Letter(c, _) => {
                let next = rest.chars().next()?;
                if !text_symbol_accepts(*c, next) {
                    return None;
                }
                out.push(next);
                rest = &rest[next.len_utf8()..];
            }
        }
    }
    rest.is_empty().then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn en() -> Locale {
        Locale::en_us()
    }

    /// The decimal comma and grouping point of `it_IT` and `de_DE`.
    fn comma_locale() -> Locale {
        Locale {
            name: "it_IT".to_string(),
            decimal: ",".to_string(),
            grouping: ".".to_string(),
            month_names: Locale::en_us().month_names.map(|m| m.to_lowercase()),
            date_patterns: vec![("long".to_string(), "DD MMMM YYYY".to_string())],
            ..Locale::en_us()
        }
    }

    fn fmt(picture: &str, raw: &str, locale: &Locale) -> Option<String> {
        Picture::parse(picture).expect("picture").format(raw, locale)
    }

    fn read(picture: &str, input: &str, locale: &Locale) -> Option<String> {
        Picture::parse(picture).expect("picture").parse_input(input, locale)
    }

    #[test]
    fn the_corpus_date_picture_round_trips() {
        assert_eq!(fmt("date{DD.MM.YYYY}", "2024-01-05", &en()).as_deref(), Some("05.01.2024"));
        assert_eq!(read("date{DD.MM.YYYY}", "05.01.2024", &en()).as_deref(), Some("2024-01-05"));
        assert_eq!(read("date{DD.MM.YYYY}", "31.02.2024", &en()), None, "no 31 February");
        assert_eq!(fmt("date{DD.MM.YYYY}", "not a date", &en()), None);
        assert_eq!(fmt("date{YYYY-MM-DD}", "20240105", &en()).as_deref(), Some("2024-01-05"));
    }

    #[test]
    fn date_names_and_named_patterns_come_from_the_locale() {
        assert_eq!(
            fmt("date{EEEE, MMMM D, YYYY}", "2024-01-05", &en()).as_deref(),
            Some("Friday, January 5, 2024")
        );
        assert_eq!(fmt("date.long{}", "2024-10-09", &comma_locale()).as_deref(), Some("09 october 2024"));
        assert_eq!(read("date{D MMM YY}", "9 Oct 24", &en()).as_deref(), Some("2024-10-09"));
        assert_eq!(read("date{D MMM YY}", "9 Oct 99", &en()).as_deref(), Some("1999-10-09"));
    }

    #[test]
    fn calendar_numbers_follow_the_spec_definitions() {
        // The spec's July 2004 example: the 1st to 3rd are week 0.
        assert_eq!(fmt("date{w}", "2004-07-03", &en()).as_deref(), Some("0"));
        assert_eq!(fmt("date{w}", "2004-07-04", &en()).as_deref(), Some("1"));
        assert_eq!(fmt("date{w}", "2004-07-31", &en()).as_deref(), Some("4"));
        assert_eq!(fmt("date{WW JJJ e E}", "2021-01-03", &en()).as_deref(), Some("53 003 7 1"));
        assert_eq!(fmt("date{WW}", "2024-12-30", &en()).as_deref(), Some("01"));
    }

    #[test]
    fn times_format_and_parse_on_both_clocks() {
        assert_eq!(fmt("time{HH:MM:SS}", "14:07:09", &en()).as_deref(), Some("14:07:09"));
        assert_eq!(fmt("time{h:MM A}", "00:30:00", &en()).as_deref(), Some("12:30 AM"));
        assert_eq!(read("time{h:MM A}", "2:05 PM", &en()).as_deref(), Some("14:05:00"));
        assert!(Picture::parse("time{HH:MM Z}").is_err(), "time zones are refused");
    }

    #[test]
    fn integers_suppress_leading_zeros_with_z_and_pad_with_9() {
        assert_eq!(fmt("num{zzzzzzzzzzzz9}", "1234", &en()).as_deref(), Some("1234"));
        assert_eq!(fmt("num{zzzzzzzzzzzz9}", "0", &en()).as_deref(), Some("0"));
        assert_eq!(fmt("num{999}", "7", &en()).as_deref(), Some("007"));
        assert_eq!(fmt("num{ZZ9}", "7", &en()).as_deref(), Some("  7"));
        assert_eq!(fmt("num{zz9}", "1234", &en()), None, "too many digits");
        assert_eq!(fmt("num{zz9}", "-5", &en()), None, "a negative needs a sign symbol");
        assert_eq!(fmt("num{szz9}", "-5", &en()).as_deref(), Some("-5"));
    }

    #[test]
    fn the_corpus_amount_alternation_picks_the_first_that_fits() {
        let picture = "num{9.99}|num{zz9.99}|num{zzz zz9.99}|num{zzz zzz zz9.99}|num{zzz zzz zzz zz9.99}";
        let it = comma_locale();
        assert_eq!(fmt(picture, "1.5", &it).as_deref(), Some("1,50"));
        assert_eq!(fmt(picture, "12.5", &it).as_deref(), Some("12,50"));
        assert_eq!(fmt(picture, "1234.5", &it).as_deref(), Some("1 234,50"));
        assert_eq!(fmt(picture, "1234567.125", &it).as_deref(), Some("1 234 567,13"), "rounded");
        assert_eq!(read(picture, "1 234,50", &it).as_deref(), Some("1234.5"));
        assert_eq!(read(picture, "12,5", &it).as_deref(), Some("12.5"));
        let grouped = "num{9.99}|num{zz9.99}|num{zzz,zz9.99}";
        assert_eq!(fmt(grouped, "1234.5", &en()).as_deref(), Some("1,234.50"));
        assert_eq!(fmt(grouped, "1234.5", &it).as_deref(), Some("1.234,50"));
    }

    #[test]
    fn a_two_position_picture_with_a_leading_nine_pads() {
        let picture = "num{9z}|num{zzzzzzzzzzzz9}";
        assert_eq!(fmt(picture, "7", &en()).as_deref(), Some("07"));
        assert_eq!(fmt(picture, "123", &en()).as_deref(), Some("123"));
    }

    #[test]
    fn fractions_follow_the_spec_examples_for_z_z_and_8() {
        // Example 26.25.
        assert_eq!(fmt("num{zz9.zzz}", "123", &en()).as_deref(), Some("123"));
        assert_eq!(fmt("num{zz9.zzz}", "1.234", &en()).as_deref(), Some("1.234"));
        assert_eq!(fmt("num{zz9.zzz}", "123.0", &en()).as_deref(), Some("123.0"));
        assert_eq!(fmt("num{ZZ9.ZZZ}", "1.234", &en()).as_deref(), Some("  1.234"));
        assert_eq!(fmt("num{ZZ9.ZZZ}", "123", &en()).as_deref(), Some("123."));
        // Example 26.26.
        assert_eq!(fmt("num{zzz,zz9.8888}", "123456.000", &en()).as_deref(), Some("123,456.000"));
        assert_eq!(fmt("num{zzz,zz9.88}", "123456", &en()).as_deref(), Some("123,456"));
        assert_eq!(fmt("num{zzz,zz9.8}", "12345.6789", &en()).as_deref(), Some("12,345.6789"));
    }

    #[test]
    fn signs_currency_and_percent_follow_example_26_28() {
        assert_eq!(fmt("num{S999V99}", "1.23", &en()).as_deref(), Some(" 001.23"));
        assert_eq!(fmt("num{S999v99}", "-1.23", &en()).as_deref(), Some("-00123"));
        assert_eq!(fmt("num{szz9.99}", "-123", &en()).as_deref(), Some("-123.00"));
        assert_eq!(fmt("num{$z,zz9.99DB}", "1234", &en()).as_deref(), Some("$1,234.00  "));
        assert_eq!(fmt("num{$z,zz9.99db}", "-1234", &en()).as_deref(), Some("$1,234.00DB"));
        assert_eq!(fmt("num{zz9%}", "0.12", &en()).as_deref(), Some("12%"));
        assert_eq!(read("num{zz9%}", "12%", &en()).as_deref(), Some("0.12"));
        assert_eq!(read("num{S999v99}", "-00123", &en()).as_deref(), Some("-1.23"));
    }

    #[test]
    fn text_pictures_place_characters_and_literals() {
        assert_eq!(fmt("text{999-999}", "123456", &en()).as_deref(), Some("123-456"));
        assert_eq!(fmt("text{999-999}", "12345", &en()), None);
        assert_eq!(read("text{999-999}", "123-456", &en()).as_deref(), Some("123456"));
        assert_eq!(fmt("text{AA 9}", "CH 1", &en()), None, "the space is a literal, not a symbol");
        assert_eq!(fmt("text{AA' '9}", "CH4", &en()).as_deref(), Some("CH 4"));
    }

    #[test]
    fn null_and_zero_clauses_match_empty_and_zero_values() {
        let picture = "null{'No data'}|text{999*9999}";
        assert!(Picture::parse(picture).is_err(), "* is a global symbol, not supported");
        let picture = "null{'none'}|zero{'nil'}|num{zz9}";
        assert_eq!(fmt(picture, "", &en()).as_deref(), Some("none"));
        assert_eq!(fmt(picture, "0", &en()).as_deref(), Some("nil"));
        assert_eq!(fmt(picture, "12", &en()).as_deref(), Some("12"));
    }

    #[test]
    fn compound_clauses_format_the_same_value_twice() {
        // Example 26.7.
        assert_eq!(
            fmt("date{DD/MM/YY} '('date{MMM DD, YYYY}')'", "1999-07-16", &en()).as_deref(),
            Some("16/07/99 (Jul 16, 1999)")
        );
    }

    #[test]
    fn a_bare_picture_infers_its_category() {
        assert_eq!(fmt("DD.MM.YYYY", "2024-01-05", &en()).as_deref(), Some("05.01.2024"));
        assert_eq!(fmt("zz9.99", "1.5", &en()).as_deref(), Some("1.50"));
        assert!(Picture::parse("---").is_err(), "nothing tells the category");
    }

    #[test]
    fn canonical_values_are_recognised_by_category() {
        assert!(is_canonical(Category::Date, "2024-01-05"));
        assert!(!is_canonical(Category::Date, "05.01.2024"));
        assert!(is_canonical(Category::Num, "-1234.5"));
        assert!(!is_canonical(Category::Num, "1.234,5"));
        assert!(is_canonical(Category::Time, "14:05:00"));
        assert!(is_canonical(Category::Text, "anything"));
    }

    #[test]
    fn malformed_pictures_are_refused() {
        assert!(Picture::parse("date{DDD}").is_err());
        assert!(Picture::parse("date{Q}").is_err());
        assert!(Picture::parse("date{DD").is_err());
        assert!(Picture::parse("num(it_IT){9}").is_err());
        assert!(Picture::parse("date.huge{}").is_err());
    }
}
