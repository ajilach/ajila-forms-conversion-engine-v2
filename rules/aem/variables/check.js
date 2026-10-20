const CODE = /^[A-Z0-9]{3,6}$/;
const ENTITY = /^[0-9]{3}$/;
// One CDOK, or the form's and its annexes' (UBS-MET-1: every CDOK is numeric).
const CDOK = /^[0-9]+(\s*[,;\/]\s*[0-9]+)*$/;
// The entity each issuer names (UBS-MET-1), matched on the whole first line of the slot2 draw,
// so another branch of UBS Europe SE ("..., Luxembourg Branch") names none of them.
const ISSUERS = [
  [/^(?:per\s+)?ubs\s+europe\s+se[\s,()\-–]*succursale\s+italia[\s,()\-–.]*$/i, "033", "UBS Europe SE (Succursale Italia)"],
  [/^ubs\s+europe\s+se[\s.]*$/i, "019", "UBS Europe SE"],
  [/^ubs\s+switzerland\s+ag[\s.]*$/i, "001", "UBS Switzerland AG"],
];

function firstLine(html) {
  const lines = String(html).split(/<\/p>|<br\s*\/?>|\n/i).map(function (l) {
    return l.replace(/<[^>]+>/g, " ").replace(/&nbsp;/g, " ").replace(/\s+/g, " ").trim();
  }).filter(function (l) { return l !== ""; });
  return lines.length > 0 ? lines[0] : "";
}

function check(output, ctx) {
  const violations = [];
  const variables = output.variables || {};
  function need(key, pattern, what) {
    const value = variables[key];
    const at = Object.prototype.hasOwnProperty.call(variables, key) ? "/variables/" + key : "/variables";
    if (typeof value !== "string" || !pattern.test(value)) {
      violations.push({
        pointer: at,
        message:
          key + " is " + (typeof value === "string" ? "\"" + value + "\"" : "missing") + "; it must be " +
          what + ", as in the source form's variables",
      });
    }
  }
  need("formrange_code", CODE, "the form code, 3 to 6 upper-case letters and digits (for example AAEV)");
  need("formrange_entity", ENTITY, "a three-digit entity code (019, 033, 001 or another entity's)");
  for (const key of ["formrange_cdokinfo", "Footer_Line_txtformid"]) {
    const value = variables[key];
    if (typeof value === "string" && value.trim() !== "" && !CDOK.test(value.trim())) {
      violations.push({
        pointer: "/variables/" + key,
        message: key + " is \"" + value + "\"; a CDOK is a number, as the footer line prints it (footerCDOK)",
      });
    }
  }

  // UBS-MET-1: the entity agrees with the issuer the page header prints.
  const entity = variables.formrange_entity;
  if (typeof entity === "string" && ENTITY.test(entity)) {
    const said = [];
    walk(output.form, function (node) {
      if (node && node.type === "TextDraw" && node.dor_header_slot === "slot2") {
        const content = node.content || {};
        for (const lang of Object.keys(content)) said.push(firstLine(content[lang]));
      }
    });
    let named = null;
    for (const text of said) {
      for (const [pattern, code, issuer] of ISSUERS) {
        if (pattern.test(text)) { named = named || [code, issuer]; break; }
      }
    }
    if (named !== null && named[0] !== entity) {
      violations.push({
        pointer: "/variables/formrange_entity",
        message: "formrange_entity is " + entity + ", and the page header names the issuer " + named[1] + ", whose " +
          "entity is " + named[0] + ": one of them is wrong; the entity is the source file name's (BADM_019_EN.pdf gives 019)",
      });
    }
  }

  // UBS-GEN-1: the form's languages are ISO 639-1 codes, Spanish as `es`.
  const languages = Array.isArray(output.languages) ? output.languages : [];
  if (languages.length === 0) {
    violations.push({ pointer: "/languages", message: "`languages` is empty: list every language of the source" });
  }
  for (let i = 0; i < languages.length; i++) {
    const lang = languages[i];
    const at = "/languages/" + i;
    if (lang === "sp") {
      violations.push({ pointer: at, message: "UBS writes Spanish `SP` in its file names and footer line; the language is `es`" });
    } else if (typeof lang !== "string" || !/^[a-z]{2}$/.test(lang)) {
      violations.push({
        pointer: at,
        message: "`" + lang + "` is no two-letter language code; a regional variant (de-ch) is the packager's, never the form's",
      });
    } else if (languages.indexOf(lang) !== i) {
      violations.push({ pointer: at, message: "`" + lang + "` is listed twice in `languages`" });
    }
  }
  return { pass: violations.length === 0, violations: violations };
}
