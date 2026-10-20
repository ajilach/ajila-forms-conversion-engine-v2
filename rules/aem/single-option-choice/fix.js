// Turns each one-option RadioButton or Dropdown into a Checkbox: same uuid, label, `hide_title`,
// option, `mandatory`, `conditions`, placement and input settings, named `CB_` instead of `RB_` or
// `DD_`, with every `copy_from` and `{NAME}` that named it renamed too. The Checkbox shows its label
// above its option as the choice did; the writer hides it where it is empty or the option's text.
const SINGLE = ["RadioButton", "Dropdown"];
const INPUTS = ["TextField", "NumberField", "DatePicker", "Dropdown", "Checkbox", "RadioButton"];

// `{NAME}` references renamed; `{{` and `}}` are braces and stay.
function renameIn(template, renames) {
  return String(template).replace(/\{\{|\}\}|\{([A-Za-z_][A-Za-z0-9_]*)\}/g, function (match, name) {
    return name !== undefined && Object.prototype.hasOwnProperty.call(renames, name) ? "{" + renames[name] + "}" : match;
  });
}

function fix(output, ctx) {
  const names = {};
  const found = [];
  function collect(node, pointer) {
    if (typeof node.name === "string") names[node.name] = true;
    if (SINGLE.indexOf(node.type) >= 0 && (node.options || []).length === 1) {
      found.push({ node: node, pointer: pointer });
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) collect(children[i], pointer + "/children/" + i);
  }
  collect(output.form, "/form");

  const renames = {};
  for (const entry of found) {
    const name = String(entry.node.name);
    const match = /^(RB|DD)_(.+)$/.exec(name);
    if (!match) continue;
    let candidate = "CB_" + match[2];
    for (let n = 2; Object.prototype.hasOwnProperty.call(names, candidate); n++) candidate = "CB_" + match[2] + "_" + n;
    names[candidate] = true;
    renames[name] = candidate;
  }

  // The node with its references renamed.
  function renamed(node) {
    const copy = JSON.parse(JSON.stringify(node));
    if (typeof copy.copy_from === "string" && Object.prototype.hasOwnProperty.call(renames, copy.copy_from)) {
      copy.copy_from = renames[copy.copy_from];
    }
    if (typeof copy.calculate === "string") copy.calculate = renameIn(copy.calculate, renames);
    return copy;
  }

  const ops = [];
  const converted = {};
  for (const entry of found) {
    const box = renamed(entry.node);
    box.type = "Checkbox";
    if (Object.prototype.hasOwnProperty.call(renames, box.name)) box.name = renames[box.name];
    if (!box.alignment) box.alignment = "Vertical";
    if (!box.conditions) box.conditions = [];
    box.options = [{ label: entry.node.options[0].label, value: "1" }];
    const old = String(entry.node.options[0].value);
    if (box.default_value !== undefined && box.default_value !== null) {
      if (String(box.default_value) === old) box.default_value = "1";
    }
    box.conditions = box.conditions.map(function (rule) {
      const value = rule.value || {};
      if ((value.type === "text" || value.type === "number") && String(value.value) === old) {
        return Object.assign({}, rule, { value: { type: "text", value: "1" } });
      }
      return rule;
    });
    converted[entry.pointer] = true;
    ops.push({ op: "replace", path: entry.pointer, value: box });
  }

  function references(node, pointer) {
    if (INPUTS.indexOf(node.type) >= 0 && !converted[pointer]) {
      const copy = renamed(node);
      if (copy.copy_from !== node.copy_from) ops.push({ op: "replace", path: pointer + "/copy_from", value: copy.copy_from });
      if (copy.calculate !== node.calculate) ops.push({ op: "replace", path: pointer + "/calculate", value: copy.calculate });
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) references(children[i], pointer + "/children/" + i);
  }
  references(output.form, "/form");
  return ops;
}
