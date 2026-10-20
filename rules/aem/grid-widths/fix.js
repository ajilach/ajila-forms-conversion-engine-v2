// Puts every width back on the grid: a fraction is rounded, a width above 12 is 12, a `colspan`
// below 1 is the full row and a `dor_colspan` below 1 is unset; an input narrower than 3 columns
// is widened to 3.
const INPUTS = ["TextField", "NumberField", "DatePicker", "Dropdown", "Checkbox", "RadioButton"];
const MIN_INPUT = 3;

function onGrid(value, fallback) {
  const rounded = Math.round(value);
  if (rounded > 12) return 12;
  if (rounded < 1) return fallback;
  return rounded;
}

function fix(output, ctx) {
  const ops = [];
  function visit(node, pointer) {
    let colspan = node.colspan;
    if (typeof colspan === "number") {
      colspan = onGrid(colspan, 12);
      if (INPUTS.indexOf(node.type) >= 0 && colspan < MIN_INPUT) colspan = MIN_INPUT;
      if (colspan !== node.colspan) ops.push({ op: "replace", path: pointer + "/colspan", value: colspan });
    }
    if (typeof node.dor_colspan === "number") {
      const dor = onGrid(node.dor_colspan, null);
      if (dor !== node.dor_colspan) ops.push({ op: "replace", path: pointer + "/dor_colspan", value: dor });
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  return ops;
}
