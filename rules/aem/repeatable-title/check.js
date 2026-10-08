const MAX_SUBJECT_WORDS = 4;
const MAX_SUBJECT_CHARS = 42;
const SUBJECT_STOP_WORDS = ["name", "nome", "nombre", "no", "nr", "number", "details", "data", "daten"];

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

// `sane_subject` of the writer: the text as a subject, or null when it names nothing usable.
function saneSubject(text) {
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
      trimmed.split(/\s+/).length > MAX_SUBJECT_WORDS ||
      SUBJECT_STOP_WORDS.indexOf(trimmed.toLowerCase()) >= 0 || trimmed.endsWith(".")) {
    return null;
  }
  return trimmed;
}

function plain(text) {
  return String(text).replace(/<[^>]+>/g, " ").replace(/&nbsp;/g, " ").replace(/\s+/g, " ").trim().toLowerCase();
}

function check(output, ctx) {
  const violations = [];
  const master = masterLanguage(output);

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

  function repeatable(node, pointer, heading, inForce) {
    const own = node.title !== undefined && masterText(node.title, master) !== node.name
      ? saneSubject(masterText(node.title, master)) : null;
    if (own === null && heading === null && inForce === null) {
      violations.push({
        pointer: pointer + "/title",
        message: "`" + node.name + "` has no usable title and nothing above it names it, so it ships the placeholder " +
          "`(Repeatable name)`: set its title to the short noun phrase of what repeats, in every language",
      });
    }
    const title = node.title || {};
    for (const draw of headings(node, pointer, [])) {
      const content = draw.node.content || {};
      for (const lang of Object.keys(content)) {
        const own = plain(title[lang] === undefined ? "" : title[lang]);
        if (own === "") continue;
        const text = plain(content[lang]);
        if (text === own || (text.indexOf(own) === 0 && /^\s+[0-9]+$/.test(text.slice(own.length)))) {
          violations.push({
            pointer: draw.pointer + "/content/" + lang,
            message: "`" + draw.node.name + "` (" + lang + ") names the row \"" + text + "\", but the repeatable `" + node.name +
              "` is titled \"" + own + "\" and the client library numbers the rows itself: delete the heading",
          });
        }
      }
    }
  }

  // `collect_add_subjects_rec`: the title in force is the nearest panel's usable title, and a
  // repeatable claims the last heading draw before it among its siblings.
  function visit(node, pointer, inherited) {
    const ownTitle = node.type === "Panel" ? saneSubject(masterText(node.title, master)) : null;
    const inForce = ownTitle !== null ? ownTitle : inherited;
    const children = node.children || [];
    let heading = null;
    for (let i = 0; i < children.length; i++) {
      const child = children[i];
      const at = pointer + "/children/" + i;
      if (child.type === "TitleDraw") heading = saneSubject(masterText(child.content, master));
      else if (child.type === "Repeatable") repeatable(child, at, heading, inForce);
      visit(child, at, inForce);
    }
  }
  visit(output.form, "/form", null);
  return { pass: violations.length === 0, violations: violations };
}
