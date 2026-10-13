function check(output, ctx) {
  const violations = [];
  const found = [];
  function visit(node, pointer, parent) {
    if (node.type === "Letterhead") found.push({ node: node, pointer: pointer, parent: parent });
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i, node);
  }
  visit(output.form, "/form", null);

  const pages = (output.form.children || []).filter(function (c) { return c.type === "Panel"; });
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
    const before = first.children.slice(0, first.children.indexOf(node));
    const other = before.filter(function (c) { return c.type !== "Preface"; });
    if (other.length > 0) {
      violations.push({
        pointer: pointer,
        message: "the letterhead `" + node.name + "` comes after `" + other[0].name + "`: it opens " +
          "the letter, so move it first on the page, after the `Preface` only",
      });
    }
  }
  return { pass: violations.length === 0, violations: violations };
}
