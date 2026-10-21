// Mirrors the refusals of the UBS writer's `check_inputs` for `enable_conditions`, so they are
// reported with the rule rather than as a failed build.
const CHOICES = ["RadioButton", "Checkbox", "Dropdown"];

function check(output, ctx) {
  const violations = [];
  const names = {};
  (function count(node) {
    if (typeof node.name === "string") names[node.name] = (names[node.name] || 0) + 1;
    (node.children || []).forEach(count);
  })(output.form);

  function visit(node, pointer) {
    const rules = node.enable_conditions || [];
    if (rules.length > 0 && CHOICES.indexOf(node.type) < 0) {
      violations.push({
        pointer: pointer + "/enable_conditions",
        message: "`" + node.name + "` is a " + node.type + ": only a radio button group, a checkbox or a " +
          "drop-down locks and unlocks other components; put the entry on the choice the source locks with",
      });
    }
    const values = (node.options || []).map(function (o) { return String(o.value); });
    rules.forEach(function (rule, i) {
      const at = pointer + "/enable_conditions/" + i;
      const target = rule.target_name;
      const found = names[target] || 0;
      if (target === node.name) {
        violations.push({ pointer: at + "/target_name", message: "`" + node.name + "` locks or unlocks itself" });
      } else if (found !== 1) {
        violations.push({
          pointer: at + "/target_name",
          message: "`" + node.name + "` locks or unlocks `" + target + "`, which names " + found +
            " components of the form rather than one: name the input or the section's Panel the source locks",
        });
      }
      const value = rule.value && rule.value.value !== undefined ? String(rule.value.value) : "";
      if (CHOICES.indexOf(node.type) >= 0 && values.indexOf(value) < 0) {
        violations.push({
          pointer: at + "/value",
          message: "`" + node.name + "` locks or unlocks `" + target + "` on `" + value + "`, which is none of " +
            "its option values (" + values.join(", ") + ")",
        });
      }
    });
    (node.children || []).forEach(function (c, i) { visit(c, pointer + "/children/" + i); });
  }
  visit(output.form, "/form");
  return { pass: violations.length === 0, violations: violations };
}
