//! The parts of Acrobat's JavaScript host that XFA forms call but the XFA
//! specification does not define: `app.alert`, `util.printd` and the dialog
//! answers of `xfa.host.messageBox`.
//!
//! XFA 3.3 leaves the host object model to the application (§10 "Scripting"
//! names `xfa.host` but not its methods). Forms authored in Designer for
//! Acrobat nevertheless call these, and a call to an undefined global throws,
//! aborting the rest of the handler. They follow Adobe's "JavaScript for
//! Acrobat API Reference" (`app.alert`, `util.printd`) and the "LiveCycle
//! Designer Scripting Reference" (`xfa.host.messageBox`).
//!
//! Only the pure parts live here; `js_helpers::XFA_HOST_EXTENSIONS` wires
//! them into the script engine.

/// A calendar date and time of day, as a script's `Date` object reports it in
/// local time. `month` is 1-based; `weekday` is 0 for Sunday, as in
/// `Date.prototype.getDay`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateParts {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub weekday: u32,
}

const MONTHS: [&str; 12] = [
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
];

const WEEKDAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

/// The format string behind `util.printd`'s numeric presets: `0` is the PDF
/// date format, `1` and `2` are the two ISO-like forms. `None` for any other
/// number.
pub fn printd_preset(preset: i64) -> Option<&'static str> {
    match preset {
        0 => Some("D:yyyymmddHHMMss"),
        1 => Some("yyyy.mm.dd HH:MM:ss"),
        2 => Some("yyyy/mm/dd HH:MM:ss"),
        _ => None,
    }
}

/// `util.printd(cFormat, oDate)`: `date` formatted by the Acrobat date
/// format string `format`.
///
/// Tokens, longest first: `mmmm` full month name, `mmm` its first three
/// letters, `mm`/`m` month number padded or not; `dddd`/`ddd` weekday name
/// full or abbreviated, `dd`/`d` day of month; `yyyy`/`yy` year; `HH`/`H`
/// 24-hour and `hh`/`h` 12-hour hour; `MM`/`M` minutes; `ss`/`s` seconds;
/// `tt`/`t` "am"/"pm" or "a"/"p". A backslash makes the next character
/// literal; every other character is copied.
pub fn printd(format: &str, date: &DateParts) -> String {
    let chars: Vec<char> = format.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            if let Some(next) = chars.get(i + 1) {
                out.push(*next);
            }
            i += 2;
            continue;
        }
        let run = chars[i..].iter().take_while(|&&x| x == c).count();
        let hour12 = match date.hour % 12 {
            0 => 12,
            h => h,
        };
        let (text, used) = match (c, run) {
            ('m', 4..) => (MONTHS[month_index(date)].to_string(), 4),
            ('m', 3) => (MONTHS[month_index(date)][..3].to_string(), 3),
            ('m', 2) => (format!("{:02}", date.month), 2),
            ('m', 1) => (date.month.to_string(), 1),
            ('d', 4..) => (WEEKDAYS[weekday_index(date)].to_string(), 4),
            ('d', 3) => (WEEKDAYS[weekday_index(date)][..3].to_string(), 3),
            ('d', 2) => (format!("{:02}", date.day), 2),
            ('d', 1) => (date.day.to_string(), 1),
            ('y', 4..) => (format!("{:04}", date.year), 4),
            ('y', 2..) => (format!("{:02}", date.year.rem_euclid(100)), 2),
            ('H', 2..) => (format!("{:02}", date.hour), 2),
            ('H', 1) => (date.hour.to_string(), 1),
            ('h', 2..) => (format!("{hour12:02}"), 2),
            ('h', 1) => (hour12.to_string(), 1),
            ('M', 2..) => (format!("{:02}", date.minute), 2),
            ('M', 1) => (date.minute.to_string(), 1),
            ('s', 2..) => (format!("{:02}", date.second), 2),
            ('s', 1) => (date.second.to_string(), 1),
            ('t', 2..) => ((if date.hour < 12 { "am" } else { "pm" }).to_string(), 2),
            ('t', 1) => ((if date.hour < 12 { "a" } else { "p" }).to_string(), 1),
            _ => (c.to_string(), 1),
        };
        out.push_str(&text);
        i += used;
    }
    out
}

fn month_index(date: &DateParts) -> usize {
    assert!(
        (1..=12).contains(&date.month),
        "month {} is outside 1..=12",
        date.month
    );
    date.month as usize - 1
}

fn weekday_index(date: &DateParts) -> usize {
    assert!(date.weekday < 7, "weekday {} is outside 0..7", date.weekday);
    date.weekday as usize
}

/// What a modal dialog answers when nobody is there to press a button: its
/// default button. `dialog_type` is Acrobat's `nType` (also the fourth
/// argument of `xfa.host.messageBox`): 0 OK, 1 OK/Cancel, 2 Yes/No,
/// 3 Yes/No/Cancel. The answer uses Acrobat's return codes, 1 for OK and 4
/// for Yes. An unknown type is treated as a plain OK dialog, as Acrobat does.
pub fn dialog_answer(dialog_type: i64) -> i32 {
    match dialog_type {
        2 | 3 => 4,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Friday 5 January 2024, 14:07:09.
    const DATE: DateParts = DateParts {
        year: 2024,
        month: 1,
        day: 5,
        hour: 14,
        minute: 7,
        second: 9,
        weekday: 5,
    };

    #[test]
    fn the_corpus_formats_pad_day_and_month() {
        assert_eq!(printd("dd.mm.yyyy", &DATE), "05.01.2024");
        assert_eq!(printd("ddmmyyyy", &DATE), "05012024");
    }

    #[test]
    fn time_tokens_distinguish_minutes_from_months_by_case() {
        assert_eq!(printd("HH:MM:ss", &DATE), "14:07:09");
        assert_eq!(printd("h:MM tt", &DATE), "2:07 pm");
        assert_eq!(printd("H:M:s t", &DATE), "14:7:9 p");
    }

    #[test]
    fn names_are_spelled_out_and_abbreviated() {
        assert_eq!(printd("dddd, mmmm d, yyyy", &DATE), "Friday, January 5, 2024");
        assert_eq!(printd("ddd mmm yy", &DATE), "Fri Jan 24");
    }

    #[test]
    fn other_characters_and_escapes_are_literal() {
        assert_eq!(printd("\\d\\a\\y d", &DATE), "day 5");
        assert_eq!(printd("[yyyy]", &DATE), "[2024]");
    }

    #[test]
    fn the_presets_name_their_format_strings() {
        assert_eq!(printd(printd_preset(0).unwrap(), &DATE), "D:20240105140709");
        assert_eq!(printd(printd_preset(2).unwrap(), &DATE), "2024/01/05 14:07:09");
        assert_eq!(printd_preset(3), None);
    }

    #[test]
    fn midnight_and_noon_are_twelve_on_a_twelve_hour_clock() {
        let midnight = DateParts { hour: 0, ..DATE };
        let noon = DateParts { hour: 12, ..DATE };
        assert_eq!(printd("hh tt", &midnight), "12 am");
        assert_eq!(printd("hh tt", &noon), "12 pm");
    }

    #[test]
    fn an_unattended_dialog_takes_its_default_button() {
        assert_eq!(dialog_answer(0), 1);
        assert_eq!(dialog_answer(1), 1);
        assert_eq!(dialog_answer(2), 4);
        assert_eq!(dialog_answer(3), 4);
    }
}
