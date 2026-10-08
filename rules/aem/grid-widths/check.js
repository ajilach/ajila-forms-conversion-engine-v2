function outside(value) {
  return typeof value === "number" && (!Number.isInteger(value) || value < 1 || value > 12);
}

function check(output, ctx) {
  const violations = [];
  function visit(node, pointer) {
    for (const field of ["colspan", "dor_colspan"]) {
      if (outside(node[field])) {
        violations.push({
          pointer: pointer + "/" + field,
          message: "`" + node.name + "` has " + field + " " + node[field] + ", outside the 12-column grid: use a whole number from 1 to 12",
        });
      }
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  return { pass: violations.length === 0, violations: violations };
}
