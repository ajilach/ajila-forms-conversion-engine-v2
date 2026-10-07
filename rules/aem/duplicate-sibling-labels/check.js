const INPUTS = ["TextField", "NumberField", "DatePicker", "Dropdown", "Checkbox", "RadioButton"];

function check(output, ctx) {
  const violations = [];
  function visit(node, pointer) {
    const children = node.children || [];
    const seen = {};
    for (let i = 0; i < children.length; i++) {
      const child = children[i];
      if (INPUTS.indexOf(child.type) < 0) continue;
      const label = child.label || {};
      for (const lang of Object.keys(label)) {
        const text = String(label[lang]).trim();
        if (text === "") continue;
        const key = lang + "\u0000" + text;
        (seen[key] = seen[key] || []).push(i);
      }
    }
    for (const key of Object.keys(seen)) {
      const indices = seen[key];
      if (indices.length < 2) continue;
      const lang = key.split("\u0000")[0];
      const text = key.slice(lang.length + 1);
      for (const i of indices) {
        violations.push({
          pointer: pointer + "/children/" + i + "/label/" + lang,
          message:
            indices.length + " inputs under `" + (node.name || "the form") + "` are all labelled \"" +
            text + "\" in " + lang,
        });
      }
    }
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  return { pass: violations.length === 0, violations: violations };
}
