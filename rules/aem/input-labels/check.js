const INPUTS = ["TextField", "NumberField", "DatePicker", "Dropdown", "Checkbox", "RadioButton"];

function problem(text) {
  const t = String(text).trim();
  if (t === "") return "is empty";
  if (t.startsWith("(") && t.endsWith(")")) return "is only a parenthetical hint";
  if (/<\/?[A-Za-z][^<>]*>/.test(t)) return "carries markup";
  return null;
}

function optionsCarryText(node, lang) {
  return (node.options || []).some(function (o) {
    const label = o.label || {};
    return Object.prototype.hasOwnProperty.call(label, lang) && String(label[lang]).trim() !== "";
  });
}

function check(output, ctx) {
  const violations = [];
  const languages = output.languages || [];
  function visit(node, pointer) {
    if (INPUTS.indexOf(node.type) >= 0) {
      const label = node.label || {};
      for (const lang of languages) {
        const exempt = node.type === "Checkbox" && optionsCarryText(node, lang);
        const text = Object.prototype.hasOwnProperty.call(label, lang) ? label[lang] : "";
        const why = problem(text);
        if (why && !(why === "is empty" && exempt)) {
          const at = Object.prototype.hasOwnProperty.call(label, lang) ? pointer + "/label/" + lang : pointer + "/label";
          violations.push({
            pointer: at,
            message: node.type + " `" + node.name + "`'s " + lang + " label " + why,
          });
        }
      }
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  return { pass: violations.length === 0, violations: violations };
}
