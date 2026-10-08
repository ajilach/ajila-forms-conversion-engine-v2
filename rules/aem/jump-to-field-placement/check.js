// What the writer counts as something to fill in (`holds_input` of `xml_writer.rs`).
const INPUTS = ["TextField", "NumberField", "DatePicker", "Dropdown", "Checkbox", "RadioButton", "Fragment"];

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

function check(output, ctx) {
  const violations = [];
  const master = masterLanguage(output);
  function visit(node, pointer) {
    if (node.type === "Panel" && node.jump_to_field === true) {
      const name = String(node.name);
      const titled = node.is_page === true &&
        masterText(node.title, master).replace(/<[^>]+>/g, "").trim() !== "";
      let message = null;
      if (name.indexOf("PN_FormConfigurator") === 0) {
        message = "`" + name + "` is the form configurator, which gets no Edit button (it would appear in the summary's jump list): delete jump_to_field";
      } else if (!(node.children || []).some(holdsInput)) {
        message = "`" + name + "` holds nothing to fill in, so an Edit button has nothing to jump to: delete jump_to_field";
      } else if (titled) {
        message = "`" + name + "` is a titled page, whose step-title panel already carries the Edit button (or its repeatables do), " +
          "so this one renders a second: delete jump_to_field";
      }
      if (message !== null) violations.push({ pointer: pointer + "/jump_to_field", message: message });
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  return { pass: violations.length === 0, violations: violations };
}
