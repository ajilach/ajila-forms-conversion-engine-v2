const CONTROLS = ["Dropdown", "Checkbox", "RadioButton"];

function check(output, ctx) {
  const violations = [];
  function visit(node, pointer) {
    if (CONTROLS.indexOf(node.type) >= 0) {
      const seen = {};
      const options = node.options || [];
      for (let i = 0; i < options.length; i++) {
        const value = options[i].value;
        const at = pointer + "/options/" + i + "/value";
        if (typeof value !== "string" || value.trim() === "") {
          violations.push({
            pointer: at,
            message: "option " + i + " of `" + node.name + "` has no value: set the source's own value",
          });
        } else if (Object.prototype.hasOwnProperty.call(seen, value)) {
          violations.push({
            pointer: at,
            message: "option " + i + " of `" + node.name + "` has the value \"" + value + "\", which option " + seen[value] +
              " has too: make them distinct as the source does",
          });
        } else {
          seen[value] = i;
        }
      }
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  return { pass: violations.length === 0, violations: violations };
}
