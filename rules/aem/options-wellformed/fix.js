// Renumbers every choice the check reports: blank entries go, the other options keep their order
// and labels and take "1" to "n", and `default_value` and the `conditions` follow their option to
// its new number. A value that names no option is left as it is, for the Author to decide.
const CHOICES = ["Dropdown", "Checkbox", "RadioButton"];

function plain(text) {
  return String(text).replace(/<[^>]*>/g, "").replace(/&nbsp;|\u00a0/g, " ").trim();
}

function blank(option) {
  const label = option.label || {};
  return !Object.keys(label).some(function (lang) { return plain(label[lang]) !== ""; });
}

function fix(output, ctx) {
  const ops = [];
  function renumber(node, pointer) {
    const options = node.options || [];
    if (options.length === 0) return;
    const kept = [];
    const renamed = {};
    let dropped = null;
    for (const option of options) {
      if (blank(option)) {
        if (dropped === null) dropped = String(option.value);
        continue;
      }
      const value = String(kept.length + 1);
      if (!Object.prototype.hasOwnProperty.call(renamed, String(option.value))) renamed[String(option.value)] = value;
      kept.push({ label: option.label, value: value });
    }
    const changed = kept.length !== options.length ||
      options.some(function (o, i) { return o.value !== kept[i].value; });
    if (changed) ops.push({ op: "replace", path: pointer + "/options", value: kept });
    if (Object.prototype.hasOwnProperty.call(node, "default_value") && node.default_value !== null) {
      const old = String(node.default_value);
      if (Object.prototype.hasOwnProperty.call(renamed, old)) {
        if (renamed[old] !== old) ops.push({ op: "replace", path: pointer + "/default_value", value: renamed[old] });
      } else if (dropped !== null && old === dropped) {
        ops.push({ op: "remove", path: pointer + "/default_value" });
      }
    }
    const conditions = node.conditions || [];
    for (let i = 0; i < conditions.length; i++) {
      const value = (conditions[i] || {}).value;
      if (!value) continue;
      const at = pointer + "/conditions/" + i + "/value";
      if (value.type === "bool") {
        // A ticked single checkbox holds its one option's value.
        if (value.value === true && kept.length === 1) ops.push({ op: "replace", path: at, value: { type: "text", value: "1" } });
        continue;
      }
      const old = String(value.value);
      if (Object.prototype.hasOwnProperty.call(renamed, old) && (renamed[old] !== old || value.type !== "text")) {
        ops.push({ op: "replace", path: at, value: { type: "text", value: renamed[old] } });
      }
    }
  }
  function visit(node, pointer) {
    if (CHOICES.indexOf(node.type) >= 0) renumber(node, pointer);
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  return ops;
}
