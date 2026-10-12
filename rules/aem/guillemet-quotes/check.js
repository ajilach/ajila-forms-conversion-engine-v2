// Whether a language code quotes with guillemets: German, Italian and Spanish, with their regional
// codes (`de-ch`) and the `sp` the UBS platform files Spanish under.
function guillemetLanguage(code) {
  const base = String(code).toLowerCase().split("-")[0];
  return base === "de" || base === "it" || base === "es" || base === "sp";
}

// The quotes that are not guillemets: the straight pair, and the curly and low ones a source PDF
// sets (`“E-Dokumente”`, `„Vorschau“`). The guard counts the straight and curly ones.
const QUOTES = /["“”„]/g;

// The text between tags, with the quote entities read as the quotes they are. A `"` inside a tag
// delimits an attribute and is dropped with the tag.
function textOf(html) {
  return String(html)
    .replace(/<[^>]*>/g, "")
    .replace(/&quot;|&#34;|&#x22;/gi, "\"")
    .replace(/&ldquo;/g, "“").replace(/&rdquo;/g, "”").replace(/&bdquo;/g, "„");
}

function isText(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function check(output, ctx) {
  const violations = [];

  function text(map, pointer, what) {
    if (!isText(map)) return;
    for (const lang of Object.keys(map)) {
      if (!guillemetLanguage(lang)) continue;
      const quotes = (textOf(map[lang]).match(QUOTES) || []).length;
      if (quotes === 0) continue;
      violations.push({
        pointer: pointer + "/" + lang,
        message: what + " (" + lang + ") quotes with " + quotes + " quote mark" +
          (quotes === 1 ? "" : "s") + " that are not guillemets: write each quoted run as «…»" +
          (quotes % 2 === 1 ? " (an odd count does not pair by position, so read the sentence first)" : ""),
      });
    }
  }

  // Every text that reaches a dictionary. An HtmlDisplayer's markup is its own locale content,
  // not a dictionary message, and is left out.
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
