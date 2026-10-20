// The quotation marks of a language as the country of the form's issuing entity writes them
// (UBS-TXT-2). The outer pair is the one `quote_marks` of the UBS layer prints the profile's own
// texts with (`src/aem/quotes.rs`); the inner pair is the one a quotation inside a quotation takes.
const SWISS_ENTITY = "001";

function marks(lang, entity) {
  const base = String(lang).toLowerCase().split(/[-_]/)[0];
  const swiss = String(entity === undefined || entity === null ? "" : entity).trim() === SWISS_ENTITY;
  if (base === "de") return swiss ? { outer: ["«", "»"], inner: ["‹", "›"] } : { outer: ["„", "“"], inner: ["‚", "‘"] };
  if (base === "it") return swiss ? { outer: ["„", "“"], inner: ["«", "»"] } : { outer: ["«", "»"], inner: ["“", "”"] };
  if (base === "fr") return swiss ? { outer: ["«", "»"], inner: ["‹", "›"] } : { outer: ["«", "»"], inner: ["“", "”"], spaced: true };
  if (base === "es" || base === "sp") return { outer: ["«", "»"], inner: ["“", "”"] };
  if (base === "en") return { outer: ["“", "”"], inner: ["‘", "’"] };
  return null;
}

// Every typographic quotation mark but `’`, which is also the apostrophe of every language here.
const QUOTE_MARKS = "«»‹›„“”‚‘‛‟";
const NO_BREAK = /[\u00a0\u202f]/;
const TAG = /<\/?[A-Za-z][^>]*>|<!--[\s\S]*?-->/g;
const NAMED = {
  quot: "\"", apos: "'", laquo: "«", raquo: "»", lsaquo: "‹", rsaquo: "›", bdquo: "„", ldquo: "“",
  rdquo: "”", sbquo: "‚", lsquo: "‘", rsquo: "’", nbsp: "\u00a0", amp: "&", lt: "<", gt: ">",
};

// The text a reader sees: tags dropped (a `"` in a tag delimits an attribute), entities read.
function textOf(html) {
  return String(html).replace(TAG, "").replace(/&(#x[0-9a-f]+|#[0-9]+|[a-z]+);/gi, function (all, name) {
    if (name[0] === "#") {
      const code = name[1] === "x" || name[1] === "X" ? parseInt(name.slice(2), 16) : parseInt(name.slice(1), 10);
      return isNaN(code) ? all : String.fromCharCode(code);
    }
    const known = NAMED[name.toLowerCase()];
    return known === undefined ? all : known;
  });
}

// A straight single quote that opens a quotation: at the start of the text or after a space or an
// opening bracket, and before a character. An apostrophe sits after a letter (`l'attivazione`,
// `QI's`, `po'`).
const OPENING_SINGLE = /(^|[\s(\[{\u2013\u2014\/])'(?=[^\s'])/g;

function count(text, re) {
  return (text.match(re) || []).length;
}

// Whether the marks of one pair alternate open, close, open, close and end closed.
function balanced(text, pair) {
  let open = false;
  for (const ch of text) {
    if (ch === pair[0]) {
      if (open) return false;
      open = true;
    } else if (ch === pair[1]) {
      if (!open) return false;
      open = false;
    }
  }
  return !open;
}

// What is wrong with the quotation marks of one text, as a list of phrases (empty when nothing).
function problems(text, m) {
  const found = [];
  const straight = count(text, /"/g);
  if (straight > 0) {
    found.push(straight + " straight double quote" + (straight === 1 ? "" : "s") +
      (straight % 2 === 1 ? " (an odd count: read the sentence to pair them)" : ""));
  }
  const single = count(text, OPENING_SINGLE);
  if (single > 0) found.push("a straight single quote used as a quotation mark");
  const allowed = m.outer.concat(m.inner).join("");
  const foreign = [];
  for (const ch of text) {
    if (QUOTE_MARKS.indexOf(ch) >= 0 && allowed.indexOf(ch) < 0 && foreign.indexOf(ch) < 0) foreign.push(ch);
  }
  if (foreign.length > 0) found.push("the marks " + foreign.join(" ") + ", which this language does not use");
  [m.outer, m.inner].forEach(function (pair) {
    if (pair[1] === "’") return;
    if ((text.indexOf(pair[0]) >= 0 || text.indexOf(pair[1]) >= 0) && !balanced(text, pair)) {
      found.push(pair[0] + "…" + pair[1] + " not paired in order");
    }
  });
  // The inner pair quotes inside a quotation only: opened outside one, it stands for the outer pair.
  let depth = 0;
  let stray = false;
  for (const ch of text) {
    if (ch === m.outer[0]) depth = 1;
    else if (ch === m.outer[1]) depth = 0;
    else if (ch === m.inner[0] && depth === 0) stray = true;
  }
  if (stray) found.push(m.inner[0] + "…" + m.inner[1] + " outside a quotation, where " + m.outer[0] + "…" + m.outer[1] + " belongs");
  if (m.spaced) {
    let unspaced = false;
    for (let i = 0; i < text.length; i++) {
      if (text[i] === "«" && !NO_BREAK.test(text[i + 1] || "")) unspaced = true;
      if (text[i] === "»" && !NO_BREAK.test(text[i - 1] || "")) unspaced = true;
    }
    if (unspaced) found.push("« or » without a no-break space inside");
  }
  return found;
}

function isText(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function check(output, ctx) {
  const violations = [];
  const entity = (output.variables || {}).formrange_entity;

  function text(map, pointer, what) {
    if (!isText(map)) return;
    for (const lang of Object.keys(map)) {
      const m = marks(lang, entity);
      if (m === null || typeof map[lang] !== "string") continue;
      const found = problems(textOf(map[lang]), m);
      if (found.length === 0) continue;
      violations.push({
        pointer: pointer + "/" + lang,
        message: what + " (" + lang + ") has " + found.join("; ") + ": for entity " + (entity || "?") +
          " this language quotes with " + m.outer[0] + (m.spaced ? " … " : "…") + m.outer[1] + ", and " +
          m.inner[0] + "…" + m.inner[1] + " inside a quotation",
      });
    }
  }

  // Every text that reaches a dictionary. An HtmlDisplayer's markup is its own locale content, not
  // a dictionary message, and is left out.
  function visit(node, pointer) {
    const what = "`" + (node.name || node.type) + "`";
    text(node.title, pointer + "/title", what + "'s title");
    text(node.label, pointer + "/label", what + "'s label");
    if (node.type !== "HtmlDisplayer") text(node.content, pointer + "/content", what);
    const options = node.options || [];
    for (let i = 0; i < options.length; i++) {
      text(options[i].label, pointer + "/options/" + i + "/label", what + "'s option " + (i + 1));
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  return { pass: violations.length === 0, violations: violations };
}
