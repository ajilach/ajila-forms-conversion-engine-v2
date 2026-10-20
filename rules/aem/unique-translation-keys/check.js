// Mirrors the writer's dictionary keying (`emit_translations` and `emit_rich_title_translations` in
// u2s-aem-ubs-mcp `aem/translated.rs`, after `text_shape::shape_texts`): a text's translations are
// keyed by its master text, so two texts with the same master text and different translations in
// one language fail the build. Reported here, at the second text, with the fix.
const BLOCKS = ["p", "ul", "ol", "li", "table", "div", "blockquote", "h1", "h2", "h3", "h4", "h5", "h6"];
const BLOCK = new RegExp("<(" + BLOCKS.join("|") + ")\\b", "i");

function masterLanguage(output) {
  const languages = output.languages || [];
  return languages.indexOf("en") >= 0 ? "en" : languages[0];
}

// The writer's `AemI18nText::master`: the master language's text, else the first language's.
function masterText(texts, master) {
  if (Object.prototype.hasOwnProperty.call(texts, master)) return String(texts[master]);
  const keys = Object.keys(texts).sort();
  return keys.length > 0 ? String(texts[keys[0]]) : "";
}

// `text_shape::paragraph`: a static text with no block element of its own is wrapped in `<p>`,
// trimmed. A TitleDraw's `title_paragraph` also turns a single other block into the paragraph,
// which changes every language alike and so no key's uniqueness; it is not mirrored.
function paragraph(value) {
  const trimmed = value.trim();
  if (trimmed === "" || BLOCK.test(trimmed)) return value;
  return "<p>" + trimmed + "</p>";
}

function isText(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

// A title as the writer also writes it, as a rich text: each non-empty language in `<p>`.
function richTitle(title) {
  const wrapped = {};
  Object.keys(title).forEach(function (lang) {
    const v = typeof title[lang] === "string" ? title[lang] : "";
    if (v !== "") wrapped[lang] = "<p>" + v + "</p>";
  });
  return wrapped;
}

function check(output, ctx) {
  const master = masterLanguage(output);
  const languages = output.languages || [];
  const violations = [];
  // Master text -> language -> { text, what } of the first text that set it.
  const dict = {};

  function emit(texts, pointer, what, shape) {
    if (!isText(texts)) return;
    const shaped = {};
    for (const lang of Object.keys(texts)) {
      const value = typeof texts[lang] === "string" ? texts[lang] : "";
      shaped[lang] = shape ? shape(value) : value;
    }
    const key = masterText(shaped, master);
    if (key === "") return;
    languages.forEach(function (lang) {
      if (lang === master || !Object.prototype.hasOwnProperty.call(shaped, lang)) return;
      const t = shaped[lang];
      if (t === "" || t === key) return;
      const entries = dict[key] || (dict[key] = {});
      const existing = entries[lang];
      if (existing === undefined) {
        entries[lang] = { text: t, what: what };
      } else if (existing.text !== t) {
        violations.push({
          pointer: pointer + "/" + lang,
          message: what + " reads " + JSON.stringify(key) + " in " + master + " like " + existing.what +
            ", but translates differently in " + lang + " (" + JSON.stringify(t) + ", not " +
            JSON.stringify(existing.text) + "), and the dictionary holds one translation per master text: " +
            "keep both translations and add a space at the end of this text's " + master + " text" +
            (shape ? ", inside its paragraph (`<p>... </p>`)" : ""),
        });
      }
    });
  }

  function visit(node, pointer) {
    const what = "`" + (node.name || node.type) + "`";
    if (node.type === "Panel" && node.is_page === true && isText(node.title)) {
      // The page's step-title draw is keyed by its rich-text form as well.
      emit(richTitle(node.title), pointer + "/title", what + "'s step title", null);
    }
    if (node.type === "Panel" || node.type === "Repeatable" || node.type === "Fragment") {
      emit(node.title, pointer + "/title", what + "'s title", null);
    }
    emit(node.label, pointer + "/label", what + "'s label", null);
    if (node.type === "TextDraw" || node.type === "TitleDraw") {
      emit(node.content, pointer + "/content", what, paragraph);
    } else if (node.type !== "HtmlDisplayer") {
      // An HtmlDisplayer carries its markup per language, outside the dictionary.
      emit(node.content, pointer + "/content", what, null);
    }
    const options = node.options || [];
    for (let i = 0; i < options.length; i++) {
      emit(options[i].label, pointer + "/options/" + i + "/label", what + "'s option " + (i + 1), null);
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }

  if (isText(output.form.title)) {
    // The form title is only written as the rich-text form title.
    emit(richTitle(output.form.title), "/form/title", "the form title", null);
  }
  const top = output.form.children || [];
  for (let i = 0; i < top.length; i++) visit(top[i], "/form/children/" + i);
  return { pass: violations.length === 0, violations: violations };
}
