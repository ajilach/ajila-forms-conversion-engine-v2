// UBS-GEN-3, CNV-INP-8, CNV-SRC-10: a choice's options are numbered "1" to "n" in source order,
// a blank placeholder is no option, and what opens the choice or a condition compares with is one
// of its option values. The writer refuses a `default_value` that is no option value
// (`check_inputs` of the UBS layer); a condition value that is none never fires.
const CHOICES = ["Dropdown", "Checkbox", "RadioButton"];

function plain(text) {
  return String(text).replace(/<[^>]*>/g, "").replace(/&nbsp;|\u00a0/g, " ").trim();
}

// An option whose label is empty in every language: a dropdown's blank first entry.
function blank(option) {
  const label = option.label || {};
  return !Object.keys(label).some(function (lang) { return plain(label[lang]) !== ""; });
}

// At most six values, quoted, for a message.
function quoted(values) {
  const shown = values.slice(0, 6).map(function (v) { return "\"" + v + "\""; });
  return shown.join(", ") + (values.length > 6 ? ", ..." : "");
}

// The value a condition compares with, as the writer writes it (`condition_value_str`).
function conditionValue(value) {
  if (!value || value.value === undefined || value.value === null) return null;
  return String(value.value);
}

function check(output, ctx) {
  const violations = [];
  function visit(node, pointer) {
    if (CHOICES.indexOf(node.type) >= 0) {
      const options = node.options || [];
      const values = [];
      if (options.length === 0) {
        violations.push({
          pointer: pointer + "/options",
          message: node.type + " `" + node.name + "` has no options, so nobody can answer it: add the source's " +
            "options, numbered \"1\" to \"n\" in source order",
        });
      }
      const wrong = [];
      for (let i = 0; i < options.length; i++) {
        if (blank(options[i])) {
          violations.push({
            pointer: pointer + "/options/" + i,
            message: "option " + i + " of `" + node.name + "` has no label in any language: a blank entry only " +
              "makes a dropdown start empty and is no option, delete it",
          });
          continue;
        }
        const expected = String(values.length + 1);
        values.push(String(options[i].value));
        if (options[i].value !== expected) wrong.push(i);
      }
      if (wrong.length > 0) {
        violations.push({
          pointer: pointer + "/options",
          message: node.type + " `" + node.name + "` has the option values " + quoted(values) + ": " +
            (values.length === 1 ? "make it \"1\"" : "number them \"1\" to \"" + values.length + "\" in source order") +
            ", keep the labels as they are, and move `default_value` and the `conditions` with their option",
        });
      }
      if (Object.prototype.hasOwnProperty.call(node, "default_value") && node.default_value !== null &&
          values.indexOf(String(node.default_value)) < 0) {
        violations.push({
          pointer: pointer + "/default_value",
          message: "`" + node.name + "` opens on \"" + node.default_value + "\", which is none of its option values (" +
            quoted(values) + "): set the value of the option the source preselects, or remove it",
        });
      }
      const conditions = node.conditions || [];
      for (let i = 0; i < conditions.length; i++) {
        const rule = conditions[i] || {};
        if (!rule.value || options.length === 0) continue;
        const at = pointer + "/conditions/" + i + "/value";
        if (rule.value.type === "bool") {
          violations.push({
            pointer: at,
            message: "a condition of `" + node.name + "` compares with " + rule.value.value + ", which no numbered " +
              "option holds, so it never fires: compare with the option's value as text (\"1\" for a ticked single " +
              "checkbox)",
          });
        } else if (values.indexOf(conditionValue(rule.value)) < 0) {
          violations.push({
            pointer: at,
            message: "a condition of `" + node.name + "` compares with \"" + conditionValue(rule.value) + "\", which is " +
              "none of its option values (" + quoted(values) + "), so it never fires: use the value of the " +
              "option meant for `" + rule.target_panel_name + "`",
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
