// A RadioButton or Dropdown with one option (a lone radio button of the source, CNV-SRC-9) is
// rendered as a Checkbox (backlog "Single-option choice"): a lone radio button cannot be cleared
// once it is chosen, and a one-entry dropdown offers no choice.
const SINGLE = ["RadioButton", "Dropdown"];

function check(output, ctx) {
  const violations = [];
  function visit(node, pointer) {
    if (SINGLE.indexOf(node.type) >= 0 && (node.options || []).length === 1) {
      violations.push({
        pointer: pointer,
        message: node.type + " `" + node.name + "` has one option: make it a Checkbox named `CB_...` with that " +
          "option (value \"1\"), keeping its label, `mandatory`, `conditions` and input settings, and rename the " +
          "`copy_from` and `{NAME}` references to it",
      });
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  return { pass: violations.length === 0, violations: violations };
}
