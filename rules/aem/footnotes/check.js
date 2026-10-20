// Mirrors the two automatable shapes of `find_footnote_noncanonical.py` (PROBLEM-footnote-modernize)
// per page, as the backlog decides (a placeholder in each page with a reference), and adds the
// rest of CNV-FN-1 a script can see: each reference has its description, every language marks the
// same footnotes, and no superscript digit is typed as a character (UBS-TXT-5).
const ACCORDION = "ubsAccordionFootnote";
const REFERENCE = /<span\b[^>]*\bdata-af-footnote-id\s*=\s*"([^"]*)"/gi;
const PARAGRAPH = /<p\b[^>]*>/gi;
const SUPERSCRIPT = /[¹²³⁴⁵⁶⁷⁸⁹⁰]/;
const RICH = ["TextDraw", "TitleDraw", "MessageBox", "HtmlDisplayer"];

function ids(re, html, pick) {
  const out = [];
  const r = new RegExp(re.source, re.flags);
  let m;
  while ((m = r.exec(html)) !== null) {
    const id = pick(m);
    if (id !== null && out.indexOf(id) < 0) out.push(id);
  }
  return out;
}

function references(html) {
  return ids(REFERENCE, html, function (m) { return m[1]; });
}

// The ids of the hidden descriptions: `<p id="ID" class="footnoteDescription">`, in any order.
function descriptions(html) {
  return ids(PARAGRAPH, html, function (m) {
    if (!/\bclass\s*=\s*"[^"]*\bfootnoteDescription\b/.test(m[0])) return null;
    const id = /\bid\s*=\s*"([^"]*)"/.exec(m[0]);
    return id ? id[1] : null;
  });
}

function isText(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function check(output, ctx) {
  const violations = [];
  let accordion = false;
  walk(output, function (node) {
    if (node !== null && typeof node === "object" && typeof node.css === "string" &&
        node.css.split(/\s+/).indexOf(ACCORDION) >= 0) accordion = true;
  });
  // A form still on accordion footnotes is the guard's other branch, which this rule does not
  // model (see rule.toml): leave it alone rather than ask for a placeholder it does not need.
  if (accordion) return { pass: true, violations: violations };

  // Every text of the form, with the page it is on (the form title counts for the first page).
  const texts = [];
  const pages = [];
  function text(map, pointer, what, rich, page) {
    if (!isText(map)) return;
    const entry = { pointer: pointer, what: what, rich: rich, langs: [] };
    for (const lang of Object.keys(map)) {
      const html = typeof map[lang] === "string" ? map[lang] : "";
      const refs = references(html);
      if (refs.length > 0) page.references.push(pointer + "/" + lang);
      entry.langs.push({ lang: lang, html: html, refs: refs, descs: descriptions(html) });
    }
    texts.push(entry);
  }
  // A Panel or Repeatable title keeps no reference, a page's included: the writer copies a page
  // title into its step-title draw without the description, and AEM fails to render a text whose
  // reference has no description in that same text (an NPE in `createMapForFootnotes`).
  function titleReference(node, pointer) {
    if (!isText(node.title)) return false;
    let found = false;
    for (const lang of Object.keys(node.title)) {
      const html = typeof node.title[lang] === "string" ? node.title[lang] : "";
      if (references(html).length === 0) continue;
      found = true;
      violations.push({
        pointer: pointer + "/title/" + lang,
        message: "`" + node.name + "`'s title (" + lang + ") carries a footnote reference, which a panel title " +
          "cannot keep: put the reference after the word that carries it in the section's first text, with its " +
          "hidden description in that same text",
      });
    }
    return found;
  }
  function visit(node, pointer, page) {
    const what = "`" + (node.name || node.type) + "`";
    if (node.type === "FootnotePlaceholder") page.placeholders.push(pointer);
    const panel = node.type === "Panel" || node.type === "Repeatable";
    if (!(panel && titleReference(node, pointer))) {
      text(node.title, pointer + "/title", what + "'s title", false, page);
    }
    text(node.label, pointer + "/label", what + "'s label", false, page);
    text(node.content, pointer + "/content", what, RICH.indexOf(node.type) >= 0, page);
    const options = node.options || [];
    for (let i = 0; i < options.length; i++) {
      text(options[i].label, pointer + "/options/" + i + "/label", what + "'s option " + (i + 1), false, page);
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i, page);
  }
  const top = output.form.children || [];
  for (let i = 0; i < top.length; i++) {
    const page = { name: top[i].name, references: [], placeholders: [] };
    if (i === 0) text(output.form.title, "/form/title", "the form title", false, page);
    visit(top[i], "/form/children/" + i, page);
    pages.push(page);
  }

  // A reference and its description sit in the same text; one in a label or an option label may
  // have its description in another text of the same language.
  const described = {};
  const referenced = {};
  texts.forEach(function (t) {
    t.langs.forEach(function (l) {
      l.descs.forEach(function (id) { described[l.lang + " " + id] = true; });
      l.refs.forEach(function (id) { referenced[l.lang + " " + id] = true; });
    });
  });
  texts.forEach(function (t) {
    let first = null;
    t.langs.forEach(function (l) {
      const at = t.pointer + "/" + l.lang;
      const undescribed = l.refs.filter(function (id) {
        return l.descs.indexOf(id) < 0 && (t.rich || !described[l.lang + " " + id]);
      });
      const unreferenced = l.descs.filter(function (id) {
        return l.refs.indexOf(id) < 0 && !referenced[l.lang + " " + id];
      });
      if (undescribed.length > 0) {
        violations.push({
          pointer: at,
          message: t.what + " (" + l.lang + ") references footnote " + undescribed.join(", ") + " with no hidden " +
            "description after it: follow the reference with `<p id=\"…\" class=\"footnoteDescription\" style=\"display: none;\">" +
            "<span class=\"footnoteDescText\">…</span></p>`",
        });
      }
      if (unreferenced.length > 0) {
        violations.push({
          pointer: at,
          message: t.what + " (" + l.lang + ") describes footnote " + unreferenced.join(", ") +
            " that no text references: add the reference where the source shows the marker, or drop the description",
        });
      }
      const key = l.refs.slice().sort().join(", ");
      if (first === null) first = { lang: l.lang, key: key };
      else if (key !== first.key) {
        violations.push({
          pointer: at,
          message: t.what + " marks footnotes [" + key + "] in " + l.lang + " but [" + first.key + "] in " + first.lang +
            ": every language marks the same footnotes",
        });
      }
      if (t.rich && SUPERSCRIPT.test(l.html.replace(/<[^>]*>/g, ""))) {
        violations.push({
          pointer: at,
          message: t.what + " (" + l.lang + ") types a superscript digit (¹ ² ³): a footnote marker is a footnote " +
            "reference, any other superscript is `<sup>`",
        });
      }
    });
  });

  // The writer renders a page's footnotes through a placeholder on that page: one on each page with
  // a reference, none on a page without.
  pages.forEach(function (page) {
    if (page.references.length > 0 && page.placeholders.length === 0) {
      page.references.forEach(function (pointer) {
        violations.push({
          pointer: pointer,
          message: "this text carries a footnote reference but its page `" + page.name + "` has no FootnotePlaceholder " +
            "node: add one to that page, usually at its end",
        });
      });
    }
    if (page.references.length === 0) {
      page.placeholders.forEach(function (pointer) {
        violations.push({
          pointer: pointer,
          message: "page `" + page.name + "` has a FootnotePlaceholder but no text on it carries a footnote reference " +
            "(data-af-footnote-id): delete the node",
        });
      });
    } else {
      page.placeholders.slice(1).forEach(function (pointer) {
        violations.push({ pointer: pointer, message: "page `" + page.name + "` already has a FootnotePlaceholder: delete this one" });
      });
    }
  });
  return { pass: violations.length === 0, violations: violations };
}
