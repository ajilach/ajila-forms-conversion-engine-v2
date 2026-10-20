const TRIGGERS = ["Dropdown", "Checkbox", "RadioButton"];

function check(output, ctx) {
  const violations = [];
  const byName = {};
  const nodes = [];
  function index(node, pointer) {
    nodes.push({ node: node, pointer: pointer });
    if (typeof node.name === "string" && node.name !== "") {
      (byName[node.name] = byName[node.name] || []).push({ node: node, pointer: pointer });
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) index(children[i], pointer + "/children/" + i);
  }
  index(output.form, "/form");

  const flagged = {};
  for (const entry of nodes) {
    const node = entry.node;
    if (TRIGGERS.indexOf(node.type) < 0) continue;
    const conditions = node.conditions || [];
    for (let i = 0; i < conditions.length; i++) {
      const rule = conditions[i];
      const at = entry.pointer + "/conditions/" + i;
      const targets = byName[rule.target_panel_name] || [];
      if (targets.length !== 1 || targets[0].node.type !== "Panel") {
        violations.push({
          pointer: at + "/target_panel_name",
          message: targets.length === 0
            ? "`" + node.name + "` targets `" + rule.target_panel_name + "`, which no node of the form is named: correct the name or rename the panel back"
            : targets.length > 1
              ? "`" + node.name + "` targets `" + rule.target_panel_name + "`, which " + targets.length + " nodes are named: the name must be unique"
              : "`" + node.name + "` targets `" + rule.target_panel_name + "`, which is a " + targets[0].node.type + ", not a Panel: wrap it in a Panel",
        });
        continue;
      }
      const target = targets[0];
      if (entry.pointer.indexOf(target.pointer + "/") === 0) {
        violations.push({
          pointer: at + "/target_panel_name",
          message: "`" + node.name + "` sits inside `" + target.node.name + "`, which its condition shows or hides: once " +
            "the panel is hidden nobody can reach the choice again; put the choice before the panel it decides",
        });
      } else if (rule.show === true && target.node.is_conditional !== true && !flagged[target.pointer]) {
        flagged[target.pointer] = true;
        violations.push({
          pointer: target.pointer + "/is_conditional",
          message: "`" + target.node.name + "` is shown by `" + node.name + "` but is not conditional, so the writer " +
            "gives it no visibility hook and it never appears: set is_conditional to true",
        });
      }
    }
  }
  return { pass: violations.length === 0, violations: violations };
}
