// What the writer counts as something to fill in (`holds_input` of `xml_writer.rs`).
const INPUTS = ["TextField", "NumberField", "DatePicker", "Dropdown", "Checkbox", "RadioButton", "Fragment", "Letterhead"];

function masterLanguage(output) {
  const languages = output.languages || [];
  return languages.indexOf("en") >= 0 ? "en" : languages[0];
}

function masterText(texts, master) {
  const map = texts || {};
  if (Object.prototype.hasOwnProperty.call(map, master)) return String(map[master]);
  const keys = Object.keys(map).sort();
  return keys.length > 0 ? String(map[keys[0]]) : "";
}

function holdsInput(node) {
  if (INPUTS.indexOf(node.type) >= 0) return true;
  return (node.children || []).some(holdsInput);
}

function holdsRepeatable(node) {
  return (node.children || []).some(function (c) { return c.type === "Repeatable" || holdsRepeatable(c); });
}

function check(output, ctx) {
  const violations = [];
  const master = masterLanguage(output);
  const titled = function (node) {
    return masterText(node.title, master).replace(/<[^>]+>/g, "").trim() !== "";
  };
  // `page`: the enclosing page, `inRow`: whether a repeatable encloses the node.
  function visit(node, pointer, page, inRow) {
    if (node.jump_to_field === true) {
      const name = "`" + node.name + "`";
      let message = null;
      if (node.type !== "Panel" || node.is_conditional === true) {
        message = name + " is " + (node.type === "Panel" ? "a conditional Panel" : "a " + node.type) +
          ", on which the writer does not write jump_to_field: delete it, the writer places the Edit buttons";
      } else if (String(node.name).indexOf("PN_FormConfigurator") === 0) {
        message = name + " is the form configurator, which gets no Edit button (it would appear in the summary's jump list): delete jump_to_field";
      } else if (!(node.children || []).some(holdsInput)) {
        message = name + " holds nothing to fill in, so an Edit button has nothing to jump to: delete jump_to_field";
      } else if (node.is_page === true && titled(node)) {
        message = name + " is a titled page, whose step-title panel already carries the Edit button (or its repeatables' rows do), " +
          "so this one renders a second: delete jump_to_field";
      } else if (inRow || holdsRepeatable(node)) {
        message = name + (inRow ? " sits in a repeatable" : " holds a repeatable") +
          ", whose rows each carry an Edit button from the writer: delete jump_to_field";
      } else if (page !== null && titled(page) && !holdsRepeatable(page) && String(page.name).indexOf("PN_FormConfigurator") !== 0) {
        message = name + " sits on the titled page `" + page.name + "`, whose step-title panel already carries the Edit button: " +
          "delete jump_to_field";
      }
      if (message !== null) violations.push({ pointer: pointer + "/jump_to_field", message: message });
    }
    const children = node.children || [];
    const ownPage = node.type === "Panel" && node.is_page === true && page === null ? node : page;
    for (let i = 0; i < children.length; i++) {
      visit(children[i], pointer + "/children/" + i, node.type === "Root" ? null : ownPage, inRow || node.type === "Repeatable");
    }
  }
  visit(output.form, "/form", null, false);
  return { pass: violations.length === 0, violations: violations };
}
