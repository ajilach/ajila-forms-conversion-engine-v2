// The languages that quote with guillemets, as a document names them.
const GUILLEMET_LANGUAGES = ["de", "it", "es"];
const STATIC_TYPES = ["TextDraw", "TitleDraw"];

// The quotes that are not guillemets: the straight pair, and the curly and low ones a source PDF
// sets (`“E-Dokumente”`, `„Vorschau“`). The guard counts the straight and curly ones.
const QUOTES = /["“”„]/g;

// The text between tags, with `&quot;` read as the quote it is. A `"` inside a tag delimits an
// attribute and is dropped with the tag.
function textOf(html) {
  return String(html).replace(/<[^>]*>/g, "").replace(/&quot;/g, "\"");
}

function check(output, ctx) {
  const violations = [];
  function visit(node, pointer) {
    if (STATIC_TYPES.indexOf(node.type) >= 0) {
      const content = node.content || {};
      for (const lang of GUILLEMET_LANGUAGES) {
        if (!Object.prototype.hasOwnProperty.call(content, lang)) continue;
        const quotes = (textOf(content[lang]).match(QUOTES) || []).length;
        if (quotes === 0) continue;
        violations.push({
          pointer: pointer + "/content/" + lang,
          message: "`" + node.name + "` (" + lang + ") quotes with " + quotes + " quote mark" +
            (quotes === 1 ? "" : "s") + " that are not guillemets: write each quoted run as «…»" +
            (quotes % 2 === 1 ? " (an odd count does not pair by position, so read the sentence first)" : ""),
        });
      }
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  return { pass: violations.length === 0, violations: violations };
}
