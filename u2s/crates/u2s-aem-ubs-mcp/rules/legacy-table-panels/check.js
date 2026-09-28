function check(output, ctx) {
  const violations = [];
  function visit(node, pointer) {
    const children = node.children || [];
    if (node.type === "Panel" && typeof node.name === "string" && node.name.startsWith("TBL_") &&
        children.length > 0 &&
        children.every(function (c) { return c.type === "TextDraw" || c.type === "TitleDraw"; })) {
      violations.push({
        pointer: pointer,
        message: "panel `" + node.name + "` lays a table out as static text; make it an HtmlDisplayer",
      });
    }
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  return { pass: violations.length === 0, violations: violations };
}
