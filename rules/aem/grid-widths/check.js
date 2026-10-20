// The 12-column grid, and the minimum width of an input on it (backlog "Minimum widths": an input
// takes at least a quarter of the row, 3 columns).
const INPUTS = ["TextField", "NumberField", "DatePicker", "Dropdown", "Checkbox", "RadioButton"];
const MIN_INPUT = 3;

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
    if (INPUTS.indexOf(node.type) >= 0 && !outside(node.colspan) && typeof node.colspan === "number" && node.colspan < MIN_INPUT) {
      violations.push({
        pointer: pointer + "/colspan",
        message: node.type + " `" + node.name + "` is " + node.colspan + " column" + (node.colspan === 1 ? "" : "s") +
          " wide: an input takes at least " + MIN_INPUT + " of the 12, so set colspan " + MIN_INPUT +
          " or more (a row that then exceeds 12 wraps)",
      });
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  return { pass: violations.length === 0, violations: violations };
}
