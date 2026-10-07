// Copied from `configurator_reset.py` / `xml_writer.rs`; see rule.toml.
const APPROVED = [
  ["Individual", "Company/Entity"],
  ["Private Person", "Minderjährige", "Firma", "GbR"],
  ["Individual", "Legal Entity"],
  ["Individuo", "Entità giuridica"],
  ["Individuale", "Persona giuridica / Società / Ditta"],
  ["Private Person", "Firma"],
  ["Private Person", "Minderjährige", "Firma"],
  ["Individual", "Legal entity"],
  ["Persona", "Persona giuridica"],
  ["Individual", "Corporate"],
  ["Private Person", "Minderjährige"],
  ["For financial institutions", "For natural persons"],
].map(function (set) { return set.map(function (l) { return l.toLowerCase(); }).join("\u0000"); });

// What a reset clears: the node types the detector counts as a field, a fragment or a repeatable.
const CLEARABLE = [
  "TextField", "NumberField", "DatePicker", "Dropdown", "Checkbox", "RadioButton", "HtmlDisplayer",
  "FootnotePlaceholder", "Fragment", "Preface", "Repeatable",
];
const CHOICES = ["RadioButton", "Dropdown"];

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

function plain(text) {
  return text.replace(/<[^>]+>/g, "").replace(/\s+/g, " ").trim().toLowerCase();
}

function showTargets(node) {
  const seen = [];
  (node.conditions || []).forEach(function (rule) {
    if (rule.show && seen.indexOf(rule.target_panel_name) < 0) seen.push(rule.target_panel_name);
  });
  return seen;
}

function check(output, ctx) {
  const violations = [];
  const master = masterLanguage(output);
  const nodes = []; // {node, pointer, children pointers via descent}
  const counts = {}; // name -> how many nodes carry it
  const panels = {}; // name -> first {node, pointer} of type Panel
  function index(node, pointer) {
    nodes.push({ node: node, pointer: pointer });
    if (typeof node.name === "string" && node.name !== "") {
      counts[node.name] = (counts[node.name] || 0) + 1;
      if (node.type === "Panel" && !panels[node.name]) panels[node.name] = { node: node, pointer: pointer };
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) index(children[i], pointer + "/children/" + i);
  }
  index(output.form, "/form");

  // The conditional panels a choice shows, in first-seen order.
  function drivenPanels(choice) {
    return showTargets(choice)
      .filter(function (name) { return panels[name] && panels[name].node.is_conditional === true; })
      .map(function (name) { return panels[name]; });
  }

  function descendants(node, pointer, out) {
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) {
      out.push({ node: children[i], pointer: pointer + "/children/" + i });
      descendants(children[i], pointer + "/children/" + i, out);
    }
    return out;
  }

  nodes.forEach(function (entry) {
    const choice = entry.node;
    if (CHOICES.indexOf(choice.type) < 0) return;
    const key = (choice.options || []).map(function (o) { return plain(masterText(o.label, master)); }).join("\u0000");
    if (APPROVED.indexOf(key) < 0) return;

    const targets = showTargets(choice);
    const resolved = targets.filter(function (name) { return panels[name]; });
    if (resolved.length < 2) {
      violations.push({
        pointer: entry.pointer,
        message:
          choice.type + " `" + choice.name + "` offers a form configurator's options but shows " +
          resolved.length + " panel(s) (show: true conditions naming a Panel in the tree)" +
          (targets.length > resolved.length ? "; no Panel is named " +
            targets.filter(function (n) { return !panels[n]; }).join(", ") : "") +
          ": give each option its conditional panel, or reword the options if this is an ordinary question",
      });
      return;
    }
    resolved.forEach(function (name) {
      const target = panels[name];
      if (target.node.is_conditional !== true) {
        violations.push({
          pointer: target.pointer + "/is_conditional",
          message:
            "panel `" + name + "` is shown by the configurator `" + choice.name +
            "`, so it must be a conditional panel (is_conditional: true), or the reset script cannot be tied to it",
        });
        return;
      }
      if (counts[name] > 1) {
        violations.push({
          pointer: target.pointer + "/name",
          message:
            "panel `" + name + "` is shown by the configurator `" + choice.name + "` but " + counts[name] +
            " nodes carry that name; the reset script names the panel, so rename the others (and any condition naming them)",
        });
      }
      const inside = descendants(target.node, target.pointer, []);
      if (!inside.some(function (d) { return CLEARABLE.indexOf(d.node.type) >= 0; })) {
        violations.push({
          pointer: target.pointer,
          message:
            "panel `" + name + "` is shown by the configurator `" + choice.name +
            "` but holds nothing a reset could clear (no input, HtmlDisplayer, fragment or repeatable): move its text out of the panel or into the panel that holds the inputs",
        });
      }
      inside.forEach(function (d) {
        if (CHOICES.indexOf(d.node.type) >= 0 && drivenPanels(d.node).length >= 2) {
          violations.push({
            pointer: d.pointer,
            message:
              d.node.type + " `" + d.node.name + "` decides which panels are shown and sits inside panel `" +
              name + "`, which the configurator `" + choice.name +
              "` resets: move it out of that panel, or the reset discards its value",
          });
        }
      });
    });
  });
  return { pass: violations.length === 0, violations: violations };
}
