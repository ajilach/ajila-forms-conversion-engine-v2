const CONTENT_TYPES = ["TextDraw", "TitleDraw", "MessageBox", "HtmlDisplayer"];
const STYLE_ATTRIBUTE = /style\s*=\s*("([^"]*)"|'([^']*)')/gi;
// A multi-column property at the start of a declaration, so `grid-template-columns` is not one.
const CSS_COLUMNS = /(^|[;\s])(-webkit-|-moz-)?(columns|column-count|column-width)\s*:/i;

function hasCssColumns(html) {
  let match;
  STYLE_ATTRIBUTE.lastIndex = 0;
  while ((match = STYLE_ATTRIBUTE.exec(html)) !== null) {
    if (CSS_COLUMNS.test(match[2] !== undefined ? match[2] : match[3])) return true;
  }
  return false;
}

function isContent(node) {
  return CONTENT_TYPES.indexOf(node.type) !== -1;
}

function narrow(value) {
  return typeof value === "number" && value !== 12;
}

// A panel whose subtree holds content and nothing else.
function onlyText(node) {
  const children = node.children || [];
  return children.length > 0 && children.every(function (c) {
    return isContent(c) || (c.type === "Panel" && onlyText(c));
  });
}

function check(output, ctx) {
  const violations = [];
  function visit(node, pointer) {
    if (isContent(node)) {
      for (const field of ["colspan", "dor_colspan"]) {
        if (narrow(node[field])) {
          violations.push({
            pointer: pointer + "/" + field,
            message: "`" + node.name + "` has " + field + " " + node[field] + ": AEM has no multi-column text, make it 12",
          });
        }
      }
    }
    if (node.type === "HtmlDisplayer" && node.content && typeof node.content === "object") {
      for (const language of Object.keys(node.content)) {
        const html = node.content[language];
        if (typeof html === "string" && hasCssColumns(html)) {
          violations.push({
            pointer: pointer + "/content/" + language,
            message: "`" + node.name + "` sets its " + language + " text in CSS columns: AEM has no multi-column text, write one column",
          });
        }
      }
    }
    if (node.type === "Panel" && narrow(node.colspan) && onlyText(node)) {
      violations.push({
        pointer: pointer,
        message: "panel `" + node.name + "` is a column of text at width " + node.colspan + ": AEM has no multi-column text, move its texts into one full-width column",
      });
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  return { pass: violations.length === 0, violations: violations };
}
