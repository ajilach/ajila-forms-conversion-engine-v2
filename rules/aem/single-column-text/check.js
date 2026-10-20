const CONTENT_TYPES = ["TextDraw", "TitleDraw", "MessageBox", "HtmlDisplayer"];
const INPUTS = ["TextField", "NumberField", "DatePicker", "Dropdown", "Checkbox", "RadioButton"];
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

// For each child, whether the grid row it lands on (12 columns, filled left to right) holds an
// input: a short fixed text beside an input keeps its place on the input's row (UBS-TXT-6).
function rowsWithInput(children) {
  const rowOf = [];
  const hasInput = [];
  let row = 0;
  let used = 0;
  for (let i = 0; i < children.length; i++) {
    const width = typeof children[i].colspan === "number" && children[i].colspan > 0 ? children[i].colspan : 12;
    if (used > 0 && used + width > 12) {
      row++;
      used = 0;
    }
    used += width;
    rowOf.push(row);
    if (INPUTS.indexOf(children[i].type) >= 0) hasInput[row] = true;
  }
  return rowOf.map(function (r) { return hasInput[r] === true; });
}

function check(output, ctx) {
  const violations = [];
  function visit(node, pointer, besideInput) {
    if (isContent(node) && !besideInput) {
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
    const beside = rowsWithInput(children);
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i, beside[i]);
  }
  visit(output.form, "/form", false);
  return { pass: violations.length === 0, violations: violations };
}
