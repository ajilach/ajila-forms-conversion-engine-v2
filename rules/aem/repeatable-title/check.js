// `subject_of` of the UBS writer (`xml_writer.rs`): the longest own title it takes as a repeatable's
// subject (`MAX_SUBJECT_WORDS`, `MAX_SUBJECT_CHARS`), and for a repeatable of signatures, named after
// who signs, often an entity line (`MAX_SIGNATURE_SUBJECT_WORDS`). Both within UBS-REP-1's six words.
const MAX_SUBJECT_WORDS = 6;
const MAX_SIGNATURE_SUBJECT_WORDS = 8;
const MAX_SUBJECT_CHARS = 60;
const SIGNATURE_GENERIC = "affrg_SignatureGeneric1";
const SUBJECT_STOP_WORDS = ["name", "nome", "nombre", "no", "nr", "number", "details", "data", "daten"];
// A plural or optional-plural mark on a word: "holder(s)", "Bevollmächtigte(r)", "Legale/i", "Firma/e".
const PLURAL_MARK = /[A-Za-zÀ-ÿ](?:\((?:s|e|n|r|en|es|i|nen|innen)\)|\/(?:s|e|n|en|es|i|nen|innen)(?![A-Za-zÀ-ÿ]))/i;
const PLURAL_SUFFIXES = ["s", "es", "n", "en", "e", "r"];

function masterLanguage(output) {
  const languages = output.languages || [];
  return languages.indexOf("en") >= 0 ? "en" : languages[0];
}

// The master text, or the first language's when the master has none (`AemI18nText::master`).
function masterText(texts, master) {
  const map = texts || {};
  if (Object.prototype.hasOwnProperty.call(map, master)) return String(map[master]);
  const keys = Object.keys(map).sort();
  return keys.length > 0 ? String(map[keys[0]]) : "";
}

function stripMarkup(text) {
  return String(text).replace(/<[^>]*>/g, "").split(/\s+/).filter(function (w) { return w !== ""; }).join(" ");
}

// `subject_of` of the writer: the text as a subject, or null when it names nothing usable.
function subjectOf(text, maxWords) {
  const plain = stripMarkup(text);
  if (/^\s*Condition: /.test(plain)) return null;
  let body = plain;
  const cut = plain.search(/\s/);
  if (cut > 0) {
    const first = plain.slice(0, cut);
    if (/^[0-9.)(\-]+$/.test(first)) body = plain.slice(cut + 1);
  }
  const trimmed = body.trim().replace(/[:*0-9]+$/, "").trim();
  if (trimmed === "" || Array.from(trimmed).length > MAX_SUBJECT_CHARS ||
      trimmed.split(/\s+/).length > maxWords ||
      SUBJECT_STOP_WORDS.indexOf(trimmed.toLowerCase()) >= 0 || trimmed.endsWith(".")) {
    return null;
  }
  return trimmed;
}

// `wraps_signatures` of the writer: a direct child Fragment of the signature generic.
function wrapsSignatures(node) {
  return (node.children || []).some(function (c) {
    return c.type === "Fragment" && typeof c.frag_ref === "string" && c.frag_ref.slice(-SIGNATURE_GENERIC.length) === SIGNATURE_GENERIC;
  });
}

function plain(text) {
  return String(text).replace(/<[^>]+>/g, " ").replace(/&nbsp;/g, " ").replace(/\s+/g, " ").trim().toLowerCase();
}

// A heading or title with its plural marks and trailing colon dropped, for comparison.
function singular(text) {
  return plain(text)
    .replace(/\((?:s|e|n|r|en|es|i|nen|innen)\)/g, "")
    .replace(/\/(?:s|e|n|en|es|i|nen|innen)(?![a-zà-ÿ])/g, "")
    .replace(/[:\s]+$/, "").replace(/\s+/g, " ").trim();
}

// Whether `heading` is `title`, or its plural or collective form.
function namesTheRows(heading, title) {
  const h = singular(heading), t = singular(title);
  if (t === "" || h === "") return false;
  if (h === t) return true;
  return PLURAL_SUFFIXES.some(function (s) { return h === t + s; });
}

// The heading draws inside a repeatable, not those of a repeatable nested in it.
function headings(node, pointer, out) {
  const children = node.children || [];
  for (let i = 0; i < children.length; i++) {
    const child = children[i];
    const at = pointer + "/children/" + i;
    if (child.type === "Repeatable") continue;
    if (child.type === "TitleDraw") out.push({ node: child, pointer: at });
    headings(child, at, out);
  }
  return out;
}

// Every heading the check reports, as {node, pointer, message}; fix.js deletes them.
function headingFindings(output) {
  const found = [];
  function repeatable(node, pointer, before) {
    const title = node.title || {};
    for (const draw of headings(node, pointer, [])) {
      const content = draw.node.content || {};
      for (const lang of Object.keys(content)) {
        const own = plain(title[lang] === undefined ? "" : title[lang]);
        if (own === "") continue;
        const text = plain(content[lang]);
        if (text === own || (text.indexOf(own) === 0 && /^\s+[0-9]+$/.test(text.slice(own.length)))) {
          found.push({
            pointer: draw.pointer,
            message: "`" + draw.node.name + "` (" + lang + ") names the row \"" + text + "\", but the repeatable `" + node.name +
              "` is titled \"" + own + "\" and the client library numbers the rows itself: delete the heading (UBS-REP-2)",
          });
          break;
        }
      }
    }
    if (before && before.node.type === "TitleDraw") {
      const content = before.node.content || {};
      for (const lang of Object.keys(content)) {
        if (title[lang] === undefined) continue;
        if (namesTheRows(content[lang], title[lang])) {
          found.push({
            pointer: before.pointer,
            message: "`" + before.node.name + "` (" + lang + ") heads the repeatable `" + node.name + "` with \"" + plain(content[lang]) +
              "\", only its title \"" + plain(title[lang]) + "\" or its plural, which each row already shows: delete the heading (UBS-REP-1)",
          });
          break;
        }
      }
    }
  }
  function visit(node, pointer) {
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) {
      const at = pointer + "/children/" + i;
      if (children[i].type === "Repeatable") {
        repeatable(children[i], at, i > 0 ? { node: children[i - 1], pointer: pointer + "/children/" + (i - 1) } : null);
      }
      visit(children[i], at);
    }
  }
  visit(output.form, "/form");
  return found;
}

function check(output, ctx) {
  const violations = [];
  const master = masterLanguage(output);
  walkRepeatables(output.form, "/form", function (node, pointer) {
    const words = wrapsSignatures(node) ? MAX_SIGNATURE_SUBJECT_WORDS : MAX_SUBJECT_WORDS;
    const text = masterText(node.title, master);
    if (text === node.name || subjectOf(text, words) === null) {
      violations.push({
        pointer: pointer + "/title",
        message: "`" + node.name + "` has no usable title" + (text === "" || text === node.name ? "" : " (\"" + stripMarkup(text) + "\")") +
          ": set it to the singular noun phrase of one row, at most " + words + " words and " + MAX_SUBJECT_CHARS +
          " characters, in every language (UBS-REP-1)",
      });
      return;
    }
    const title = node.title || {};
    for (const lang of Object.keys(title)) {
      if (PLURAL_MARK.test(stripMarkup(title[lang]))) {
        violations.push({
          pointer: pointer + "/title/" + lang,
          message: "`" + node.name + "` (" + lang + ") is titled \"" + stripMarkup(title[lang]) + "\", a plural or optional-ending form; the " +
            "title names one row and the rows are numbered from it: write the singular (UBS-REP-1)",
        });
      }
    }
  });
  headingFindings(output).forEach(function (f) { violations.push({ pointer: f.pointer, message: f.message }); });
  return { pass: violations.length === 0, violations: violations };
}

function walkRepeatables(node, pointer, callback) {
  if (node.type === "Repeatable") callback(node, pointer);
  const children = node.children || [];
  for (let i = 0; i < children.length; i++) walkRepeatables(children[i], pointer + "/children/" + i, callback);
}
