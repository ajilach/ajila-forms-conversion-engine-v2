//! The locales picture clauses format with (XFA 3.3 Chapter 26,
//! "Localization and Canonicalization"): month and day names, the named date
//! and time patterns (`date.long{}`), and the number symbols. Read once from
//! the document's `<localeSet>` packet; a locale the packet does not define
//! falls back to the built-in `en_US`, the spec's ambient default.

use super::{XfaNode, XfaNodeKind};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Locale {
    pub name: String,
    pub month_names: [String; 12],
    pub month_abbrs: [String; 12],
    /// Sunday first, as `<dayNames>` lists them.
    pub day_names: [String; 7],
    pub day_abbrs: [String; 7],
    /// AM, PM.
    pub meridiems: [String; 2],
    /// BC, AD.
    pub eras: [String; 2],
    /// `full`, `long`, `med`, `short` date patterns.
    pub date_patterns: Vec<(String, String)>,
    pub time_patterns: Vec<(String, String)>,
    /// `numeric`, `currency`, `percent` number patterns.
    pub number_patterns: Vec<(String, String)>,
    pub decimal: String,
    pub grouping: String,
    pub minus: String,
    pub percent: String,
    pub currency_symbol: String,
    pub currency_iso: String,
    /// The digit zero; the other digits follow it in code point order
    /// (XFA 3.3 "The numberSymbol element", `zero`).
    pub zero: char,
}

fn strings<const N: usize>(items: [&str; N]) -> [String; N] {
    items.map(str::to_string)
}

impl Locale {
    /// `en_US`, the ambient locale when a document names no other.
    pub fn en_us() -> Locale {
        Locale {
            name: "en_US".to_string(),
            month_names: strings([
                "January",
                "February",
                "March",
                "April",
                "May",
                "June",
                "July",
                "August",
                "September",
                "October",
                "November",
                "December",
            ]),
            month_abbrs: strings([
                "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
            ]),
            day_names: strings([
                "Sunday",
                "Monday",
                "Tuesday",
                "Wednesday",
                "Thursday",
                "Friday",
                "Saturday",
            ]),
            day_abbrs: strings(["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]),
            meridiems: strings(["AM", "PM"]),
            eras: strings(["BC", "AD"]),
            date_patterns: vec![
                ("full".to_string(), "EEEE, MMMM D, YYYY".to_string()),
                ("long".to_string(), "MMMM D, YYYY".to_string()),
                ("med".to_string(), "MMM D, YYYY".to_string()),
                ("short".to_string(), "M/D/YY".to_string()),
            ],
            time_patterns: vec![
                ("full".to_string(), "h:MM:SS A Z".to_string()),
                ("long".to_string(), "h:MM:SS A Z".to_string()),
                ("med".to_string(), "h:MM:SS A".to_string()),
                ("short".to_string(), "h:MM A".to_string()),
            ],
            number_patterns: vec![
                ("numeric".to_string(), "z,zzz,zzz,zzz,zzz,zzz,zz9.zzzzzzzz".to_string()),
                ("currency".to_string(), "$z,zzz,zzz,zzz,zzz,zzz,zz9.99".to_string()),
                ("percent".to_string(), "z,zzz,zzz,zzz,zzz,zzz,zz9%".to_string()),
            ],
            decimal: ".".to_string(),
            grouping: ",".to_string(),
            minus: "-".to_string(),
            percent: "%".to_string(),
            currency_symbol: "$".to_string(),
            currency_iso: "USD".to_string(),
            zero: '0',
        }
    }

    /// The named date pattern (`full`, `long`, `med`, `short`).
    pub fn date_pattern(&self, name: &str) -> Option<&str> {
        self.date_patterns
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, p)| p.as_str())
    }

    /// The named time pattern.
    pub fn time_pattern(&self, name: &str) -> Option<&str> {
        self.time_patterns
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, p)| p.as_str())
    }

    pub fn number_pattern(&self, name: &str) -> Option<&str> {
        self.number_patterns
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, p)| p.as_str())
    }

    /// The ASCII digit `d` in this locale's digits.
    pub fn digit(&self, d: char) -> char {
        if self.zero == '0' || !d.is_ascii_digit() {
            return d;
        }
        char::from_u32(self.zero as u32 + (d as u32 - '0' as u32)).unwrap_or(d)
    }

    /// The language part of this locale's name (`de` for `de_CH`).
    pub fn language(&self) -> &str {
        language_part(&self.name)
    }

    /// A `<locale>` element, every part it leaves out taken from `en_US`.
    fn from_node(node: &XfaNode) -> Option<Locale> {
        let mut locale = Locale::en_us();
        locale.name = node.attributes.get("name")?.clone();
        for part in elements(node) {
            match tag(part) {
                "calendarSymbols" => {
                    for symbols in elements(part) {
                        let abbr = symbols.attributes.get("abbr").map(String::as_str) == Some("1");
                        let texts: Vec<String> = elements(symbols).map(text_of).collect();
                        match (tag(symbols), abbr) {
                            ("monthNames", false) => fill(&mut locale.month_names, texts),
                            ("monthNames", true) => fill(&mut locale.month_abbrs, texts),
                            ("dayNames", false) => fill(&mut locale.day_names, texts),
                            ("dayNames", true) => fill(&mut locale.day_abbrs, texts),
                            ("meridiemNames", _) => fill(&mut locale.meridiems, texts),
                            ("eraNames", _) => fill(&mut locale.eras, texts),
                            _ => {}
                        }
                    }
                }
                "datePatterns" => locale.date_patterns = named_texts(part),
                "timePatterns" => locale.time_patterns = named_texts(part),
                "numberPatterns" => locale.number_patterns = named_texts(part),
                "numberSymbols" | "currencySymbols" => {
                    let currency = tag(part) == "currencySymbols";
                    for (name, value) in named_texts(part) {
                        match (currency, name.as_str()) {
                            (false, "decimal") => locale.decimal = value,
                            (false, "grouping") => locale.grouping = value,
                            (false, "minus") => locale.minus = value,
                            (false, "percent") => locale.percent = value,
                            (false, "zero") => match value.chars().next() {
                                Some(zero) if value.chars().count() == 1 => locale.zero = zero,
                                _ => log::warn!(
                                    "locale {}: numberSymbol zero {value:?} is not one character; using 0",
                                    locale.name
                                ),
                            },
                            (true, "symbol") => locale.currency_symbol = value,
                            (true, "isoname") => locale.currency_iso = value,
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        Some(locale)
    }
}

fn tag(node: &XfaNode) -> &str {
    match &node.kind {
        XfaNodeKind::Element { tag_name, .. } => tag_name.as_str(),
        _ => "",
    }
}

fn elements(node: &XfaNode) -> impl Iterator<Item = &XfaNode> {
    node.children
        .iter()
        .filter(|c| matches!(c.kind, XfaNodeKind::Element { .. }))
}

fn text_of(node: &XfaNode) -> String {
    match &node.kind {
        XfaNodeKind::Element {
            text_content: Some(text),
            ..
        } => text.trim().to_string(),
        _ => node
            .children
            .iter()
            .filter_map(|c| match &c.kind {
                XfaNodeKind::Text { content } => Some(content.as_str()),
                _ => None,
            })
            .collect::<String>()
            .trim()
            .to_string(),
    }
}

/// Each child's `name` attribute and text.
fn named_texts(node: &XfaNode) -> Vec<(String, String)> {
    elements(node)
        .filter_map(|c| Some((c.attributes.get("name")?.clone(), text_of(c))))
        .collect()
}

/// `target` overwritten by `texts`, when there are exactly as many.
fn fill<const N: usize>(target: &mut [String; N], texts: Vec<String>) {
    if let Ok(texts) = <[String; N]>::try_from(texts) {
        *target = texts;
    }
}

/// Every locale a document's `<localeSet>` defines.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LocaleSet {
    locales: Vec<Locale>,
}

impl LocaleSet {
    /// The locales of every `<localeSet>` in `nodes`.
    pub fn from_nodes(nodes: &[XfaNode]) -> LocaleSet {
        fn walk(nodes: &[XfaNode], inside: bool, out: &mut Vec<Locale>) {
            for node in nodes {
                let here = tag(node);
                if inside && here == "locale" {
                    out.extend(Locale::from_node(node));
                    continue;
                }
                walk(&node.children, inside || here == "localeSet", out);
            }
        }
        let mut locales = Vec::new();
        walk(nodes, false, &mut locales);
        LocaleSet { locales }
    }

    /// The locale named `name`. A locale the set does not define is
    /// reported and replaced by the set's first locale of the same language
    /// (`de_DE` for `de_CH`), else by `en_US` (the ambient locale, XFA 3.3
    /// "Determining the Prevailing Locale").
    pub fn resolve(&self, name: Option<&str>) -> Locale {
        let Some(name) = name else {
            return Locale::en_us();
        };
        if let Some(found) = self.locales.iter().find(|l| l.name == name) {
            return found.clone();
        }
        let language = language_part(name);
        if let Some(same_language) = self
            .locales
            .iter()
            .find(|l| !language.is_empty() && l.language() == language)
        {
            log::warn!(
                "locale {name} is not in the document's localeSet; using {} instead",
                same_language.name
            );
            return same_language.clone();
        }
        if name != "en_US" {
            log::warn!("locale {name} is not in the document's localeSet; using en_US");
        }
        Locale::en_us()
    }
}

/// The language part of a locale tag.
fn language_part(name: &str) -> &str {
    name.split(['_', '-']).next().unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCALE_SET: &str = r#"<xdp><localeSet><locale name="it_IT"><calendarSymbols><monthNames><month>gennaio</month><month>febbraio</month><month>marzo</month><month>aprile</month><month>maggio</month><month>giugno</month><month>luglio</month><month>agosto</month><month>settembre</month><month>ottobre</month><month>novembre</month><month>dicembre</month></monthNames></calendarSymbols><datePatterns><datePattern name="long">DD MMMM YYYY</datePattern></datePatterns><numberSymbols><numberSymbol name="decimal">,</numberSymbol><numberSymbol name="grouping">.</numberSymbol></numberSymbols></locale></localeSet></xdp>"#;

    #[test]
    fn a_missing_country_falls_back_to_the_same_language_before_en_us() {
        let set = LocaleSet::from_nodes(&XfaNode::parse(LOCALE_SET.as_bytes()).expect("xml"));
        assert_eq!(set.resolve(Some("it_CH")).name, "it_IT");
        assert_eq!(set.resolve(Some("fr_CH")).name, "en_US");
        assert_eq!(set.resolve(None).name, "en_US");
    }

    #[test]
    fn number_patterns_and_the_zero_symbol_are_read() {
        let xml = r#"<xdp><localeSet><locale name="ar_EG"><numberPatterns><numberPattern name="numeric">z,zz9.zzz</numberPattern></numberPatterns><numberSymbols><numberSymbol name="zero">٠</numberSymbol></numberSymbols></locale></localeSet></xdp>"#;
        let set = LocaleSet::from_nodes(&XfaNode::parse(xml.as_bytes()).expect("xml"));
        let ar = set.resolve(Some("ar_EG"));
        assert_eq!(ar.number_pattern("numeric"), Some("z,zz9.zzz"));
        assert_eq!(ar.zero, '٠');
        assert_eq!(ar.digit('3'), '٣');
        assert_eq!(Locale::en_us().digit('3'), '3');
    }

    #[test]
    fn a_defined_locale_is_read_and_the_rest_comes_from_en_us() {
        let set = LocaleSet::from_nodes(&XfaNode::parse(LOCALE_SET.as_bytes()).expect("xml"));
        let it = set.resolve(Some("it_IT"));
        assert_eq!(it.name, "it_IT");
        assert_eq!(it.month_names[9], "ottobre");
        assert_eq!(it.month_abbrs[9], "Oct", "not in the packet, so en_US");
        assert_eq!(it.date_pattern("long"), Some("DD MMMM YYYY"));
        assert_eq!((it.decimal.as_str(), it.grouping.as_str()), (",", "."));
    }

    #[test]
    fn an_unknown_locale_is_en_us() {
        let set = LocaleSet::from_nodes(&XfaNode::parse(LOCALE_SET.as_bytes()).expect("xml"));
        assert_eq!(set.resolve(Some("fr_FR")), Locale::en_us());
        assert_eq!(set.resolve(None), Locale::en_us());
    }
}
