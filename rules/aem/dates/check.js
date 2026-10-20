// CNV-INP-3 in AEM: a full date is one DatePicker, also where the source asks day, month and year
// in separate inputs; a partial date (month and year, or the year) has no picker in AEM and is
// asked as one TextField per part; a picker opens on the source's default, `YYYY-MM-DD`.
const PARTS = ["TextField", "NumberField", "Dropdown"];
const SEPARATORS = ["TextDraw", "TitleDraw"];

// The words that name a part of a date, in the forms' languages and their format hints.
const DAY = ["day", "tag", "tt", "giorno", "gg", "dia", "día", "dd", "jour"];
const MONTH = ["month", "monat", "mm", "mese", "mes", "mois"];
const YEAR = ["year", "jahr", "jjjj", "yyyy", "aaaa", "anno", "año", "ano", "année", "annee", "yy"];

// The words of a date in the master language's label; a name is read with all of them but `data`.
const DATE_WORDS = {
  en: /(^|[^\p{L}])date($|[^\p{L}])/iu,
  fr: /(^|[^\p{L}])date($|[^\p{L}])/iu,
  de: /datum($|[^\p{L}])/iu,
  it: /(^|[^\p{L}])data($|[^\p{L}])/iu,
  es: /(^|[^\p{L}])fecha($|[^\p{L}])/iu,
};
const NAME_DATE = /(^|[^\p{L}])(date|fecha)($|[^\p{L}])|datum($|[^\p{L}])/iu;
// A label that also asks a place, a time span or a signature is not one date.
const NOT_A_DATE = /(^|[^\p{L}])(ort|place|luogo|lugar|lieu|unterschrift|signature|firma|from|to|von|bis|dal|al|desde|hasta)($|[^\p{L}])/iu;

const CAMEL = /(?<=[a-zà-ÿ0-9])(?=[A-ZÀ-Þ])|(?<=[A-ZÀ-Þ])(?=[A-ZÀ-Þ][a-zà-ÿ])/g;

function plain(text) {
  return String(text || "").replace(/<[^>]*>/g, " ").replace(/&nbsp;|\u00a0/g, " ").replace(/\s+/g, " ").trim();
}

function masterLanguage(output) {
  const languages = output.languages || [];
  return languages.indexOf("en") >= 0 ? "en" : languages[0];
}

function masterText(texts, master) {
  const map = texts || {};
  if (Object.prototype.hasOwnProperty.call(map, master)) return String(map[master]);
  const keys = Object.keys(map).sort();
  return keys.length > 0 ? String(map[keys[0]]) : "";
}

function nameWords(name) {
  return String(name || "").replace(/^[A-Z]+_/, "").replace(/_/g, " ").replace(CAMEL, " ");
}

// The text that says what an input asks: its master label, else its name.
function probe(node, master) {
  const label = plain(masterText(node.label, master));
  return label !== "" ? { text: label, fromName: false } : { text: nameWords(node.name), fromName: true };
}

function words(text) {
  return text.toLowerCase().split(/[^\p{L}]+/u).filter(function (w) { return w !== ""; });
}

// Which parts of a date a text names: some of "day", "month", "year".
function partsOf(text) {
  const found = [];
  const list = words(text);
  const has = function (vocabulary, suffix) {
    return list.some(function (w) {
      return vocabulary.indexOf(w) >= 0 || (suffix && w.length > suffix.length && w.slice(-suffix.length) === suffix);
    });
  };
  if (has(DAY, null)) found.push("day");
  if (has(MONTH, "monat")) found.push("month");
  if (has(YEAR, "jahr")) found.push("year");
  return found;
}

function validDate(value) {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(String(value));
  if (!m) return false;
  const year = Number(m[1]), month = Number(m[2]), day = Number(m[3]);
  if (month < 1 || month > 12 || day < 1) return false;
  const days = [31, (year % 4 === 0 && (year % 100 !== 0 || year % 400 === 0)) ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
  return day <= days[month - 1];
}

function check(output, ctx) {
  const violations = [];
  const master = masterLanguage(output);
  const dateWord = DATE_WORDS[String(master).split("-")[0]] || DATE_WORDS.en;

  function visit(node, pointer) {
    if (node.type === "DatePicker") {
      if (node.default_value !== undefined && node.default_value !== null && !validDate(node.default_value)) {
        violations.push({
          pointer: pointer + "/default_value",
          message: "`" + node.name + "` opens on \"" + node.default_value + "\", which is no date: write the source's " +
            "default as YYYY-MM-DD, or remove it (a picker never opens on today's date)",
        });
      }
      const parts = partsOf(probe(node, master).text);
      if (parts.length > 0 && parts.indexOf("day") < 0) {
        violations.push({
          pointer: pointer,
          message: "DatePicker `" + node.name + "` asks only the " + parts.join(" and ") + ", which a date picker " +
            "cannot: ask each part in a TextField of its own (`max_chars` 2 for a month, 4 for a year)",
        });
      }
    }
    if ((node.type === "TextField" && (!node.kind || node.kind === "Plain")) || node.type === "NumberField") {
      const asked = probe(node, master);
      const word = asked.fromName ? NAME_DATE : dateWord;
      const parts = partsOf(asked.text);
      const named = word.test(asked.text) && !NOT_A_DATE.test(asked.text) && parts.length === 0;
      if (node.read_only !== true && (named || parts.length === 3)) {
        violations.push({
          pointer: pointer,
          message: node.type + " `" + node.name + "` asks a date (\"" + asked.text + "\"): make it a DatePicker " +
            "named `DATE_...`",
        });
      }
    }
    const children = node.children || [];
    // Inputs side by side that each ask one part of a date: day, month and year are one date.
    const run = [];
    function close() {
      for (let start = 0; start + 3 <= run.length; start++) {
        const trio = run.slice(start, start + 3);
        const parts = trio.map(function (e) { return e.part; });
        if (parts.indexOf("day") >= 0 && parts.indexOf("month") >= 0 && parts.indexOf("year") >= 0) {
          violations.push({
            pointer: trio[0].pointer,
            message: "`" + trio.map(function (e) { return e.name; }).join("`, `") + "` ask the day, the month and " +
              "the year of one date: replace them by one DatePicker named `DATE_...` with the date's caption",
          });
          start += 2;
        }
      }
      run.length = 0;
    }
    for (let i = 0; i < children.length; i++) {
      const child = children[i];
      if (PARTS.indexOf(child.type) >= 0) {
        const parts = partsOf(probe(child, master).text);
        if (parts.length === 1) {
          run.push({ part: parts[0], name: child.name, pointer: pointer + "/children/" + i });
          continue;
        }
      } else if (SEPARATORS.indexOf(child.type) >= 0) {
        continue;
      }
      close();
    }
    close();
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  return { pass: violations.length === 0, violations: violations };
}
