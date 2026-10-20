// Copied from the feedback repo's `email_phone_labels.py`; see rule.toml. Python's `\b` is
// Unicode-aware and JavaScript's is not, so the boundaries are spelled as lookarounds.
const NOT_WORD_BEFORE = "(?<![\\p{L}\\p{N}_])";
const NOT_WORD_AFTER = "(?![\\p{L}\\p{N}_])";
const EMAIL_RE = new RegExp(
  NOT_WORD_BEFORE + "(e[-\\s.]?mail|courriel|posta\\s+elettronica|pec)" + NOT_WORD_AFTER, "iu");
const PHONE_RE = new RegExp(
  NOT_WORD_BEFORE +
    "(tel|telefon|telefono|telefonico|telefonica|telephone|t[ée]l[ée]phone|" +
    "tel[eé]fono|phone|mobile|mobil|handy|natel|cellulare|m[oó]vil|" +
    "fax|telefax|facsimile)" + NOT_WORD_AFTER, "iu");

// Labels containing a matching word that are not an email or phone field, matched lowercased
// as a substring of the whitespace-collapsed label.
const DENY_LABEL = [
  "telefonische bestellung",
  "telefonico canale",
  "titolare / numero di telefono",
];
// Names (prefix match) that are not email or phone fields.
const DENY_NAME = ["TXT_MailingAddressConfirmation", "TXT_Amountimobili", "TXT_TotaleImmobili"];

// A name is CamelCase with `_` separators; the humps are split so that `\btelefono\b` can match
// inside `NumeroDiTelefonoFisso`.
const CAMEL_RE = /(?<=[a-zà-ÿ0-9])(?=[A-ZÀ-Þ])|(?<=[A-ZÀ-Þ])(?=[A-ZÀ-Þ][a-zà-ÿ])/g;

// What a panel holds that Redacto types as a row of its own (`find_contact_leaf_panel.py`).
const PANELS = ["Panel", "Repeatable", "Fragment"];

function norm(text) {
  return String(text || "").replace(/\s+/g, " ").trim();
}

function nameProbe(name) {
  return norm(String(name || "").replace(/_/g, " ").replace(CAMEL_RE, " "));
}

// The language the profile masters the form in: English when the form ships it, else the first.
function masterLanguage(output) {
  const languages = output.languages || [];
  return languages.indexOf("en") >= 0 ? "en" : languages[0];
}

// The master text of a language map, as the writer takes it for `jcr:title`: the master
// language's entry, or the first entry in key order when there is none.
function masterText(texts, master) {
  const map = texts || {};
  if (Object.prototype.hasOwnProperty.call(map, master)) return String(map[master]);
  const keys = Object.keys(map).sort();
  return keys.length > 0 ? String(map[keys[0]]) : "";
}

// "Email", "Telephone" or null, as `classify` in email_phone_labels.py decides.
function classify(label, name) {
  for (const denied of DENY_NAME) {
    if (String(name || "").startsWith(denied)) return null;
  }
  const text = norm(label);
  const lower = text.toLowerCase();
  for (const denied of DENY_LABEL) {
    if (lower.indexOf(denied) >= 0) return null;
  }
  const probe = text !== "" ? text : nameProbe(name);
  if (EMAIL_RE.test(probe)) return "Email";
  if (PHONE_RE.test(probe)) return "Telephone";
  return null;
}

function isContact(node) {
  return node.type === "TextField" && (node.kind === "Email" || node.kind === "Telephone");
}

// Left out of the summary data: excluded and not put back for the PDF.
function excluded(node) {
  return (node.summary_exclude === true || node.dor_exclude === true) && node.always_in_pdf !== true;
}

// Posted to the summary (`summary-backend-creator.js::pushData`'s gate).
function emitted(node) {
  return !excluded(node) && (node.visible !== false || node.show_if_hidden === true || node.always_in_pdf === true);
}

// The runs of contact inputs, side by side, under a parent that also holds a panel: Redacto cannot
// type such a row and the whole Document of Record comes out blank.
function contactRuns(node) {
  const children = node.children || [];
  if (!children.some(function (c) { return PANELS.indexOf(c.type) >= 0 && !excluded(c); })) return [];
  const runs = [];
  let run = null;
  for (let i = 0; i < children.length; i++) {
    if (isContact(children[i])) {
      if (run === null) run = { start: i, end: i, emitted: false };
      run.end = i;
      run.emitted = run.emitted || emitted(children[i]);
    } else {
      if (run !== null && run.emitted) runs.push(run);
      run = null;
    }
  }
  if (run !== null && run.emitted) runs.push(run);
  return runs;
}

function check(output, ctx) {
  const violations = [];
  const master = masterLanguage(output);
  function visit(node, pointer, hidden) {
    const isPlainText = node.type === "TextField" && (!node.kind || node.kind === "Plain");
    // A value the form fills in itself is not asked of anybody (UBS-INP-8).
    if ((isPlainText || node.type === "NumberField") && node.read_only !== true) {
      const kind = classify(masterText(node.label, master), node.name);
      if (kind) {
        const prefix = kind === "Email" ? "EML_" : "TEL_";
        if (node.type === "TextField") {
          violations.push({
            pointer: Object.prototype.hasOwnProperty.call(node, "kind") ? pointer + "/kind" : pointer,
            message:
              "TextField `" + node.name + "` asks for " +
              (kind === "Email" ? "an email address" : "a telephone number") +
              "; set its kind to `" + kind + "` and name it `" + prefix + "...`",
          });
        } else {
          violations.push({
            pointer: pointer,
            message:
              "NumberField `" + node.name + "` asks for " +
              (kind === "Email" ? "an email address" : "a telephone number") +
              "; replace it with a TextField of kind `" + kind + "` named `" + prefix +
              "...` (a number box reformats and rejects a leading +)",
          });
        }
      }
    }
    const out = hidden || excluded(node);
    if (!out) {
      for (const run of contactRuns(node)) {
        const names = (node.children || []).slice(run.start, run.end + 1).map(function (c) { return c.name; });
        violations.push({
          pointer: pointer + "/children/" + run.start,
          message: "`" + names.join("`, `") + "` sit" + (names.length === 1 ? "s" : "") + " beside a panel in `" +
            (node.name || "the form") + "`, which empties the whole Document of Record: wrap " +
            (names.length === 1 ? "it in a Panel of its own" : "them in a Panel of their own") + " (`PN_TelephoneAndEmail`)",
        });
      }
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i, out);
  }
  visit(output.form, "/form", false);
  return { pass: violations.length === 0, violations: violations };
}
