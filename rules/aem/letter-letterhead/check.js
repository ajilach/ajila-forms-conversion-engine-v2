// The panels under the form that are not wizard steps (same list as the step-titles rule).
const SPECIAL_NAMES = ["formmetadata", "formMetadata", "FormMetadata", "metadata", "autoSaveInfo",
  "signerInfo", "doroptionsubs", "PN_Preview"];

function isPage(node) {
  const name = typeof node.name === "string" ? node.name : "";
  return node.type === "Panel" && node.is_page !== false && SPECIAL_NAMES.indexOf(name) < 0 &&
    name.toLowerCase() !== "preview";
}

function check(output, ctx) {
  const violations = [];
  const found = [];
  function visit(node, pointer, parent) {
    if (node.type === "Letterhead") found.push({ node: node, pointer: pointer, parent: parent });
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i, node);
  }
  visit(output.form, "/form", null);

  const pages = (output.form.children || []).filter(isPage);
  const first = pages.length > 0 ? pages[0] : null;
  for (let i = 0; i < found.length; i++) {
    const { node, pointer, parent } = found[i];
    if (i > 0) {
      violations.push({
        pointer: pointer,
        message: "`" + node.name + "` is a second letterhead, beside `" + found[0].node.name +
          "`: a letter has one, so delete this one",
      });
      continue;
    }
    if (parent !== first) {
      violations.push({
        pointer: pointer,
        message: "the letterhead `" + node.name + "` is not at the top of the first page: move it " +
          "among the first page's children, right after the `Preface` where there is one",
      });
      continue;
    }
    const at = first.children.indexOf(node);
    const before = first.children.slice(0, at);
    const other = before.filter(function (c) { return c.type !== "Preface"; });
    const prefaceAfter = first.children.slice(at + 1).some(function (c) { return c.type === "Preface"; });
    if (prefaceAfter) {
      violations.push({
        pointer: pointer,
        message: "the letterhead `" + node.name + "` comes before the `Preface`: the banking relationship " +
          "opens the page, so move the letterhead right after it",
      });
    } else if (other.length > 0) {
      violations.push({
        pointer: pointer,
        message: "the letterhead `" + node.name + "` comes after `" + other[0].name + "`: it opens " +
          "the letter, so move it first on the page, after the `Preface` only",
      });
    }
  }
  return { pass: violations.length === 0, violations: violations };
}
