// The UBS naming convention (`specs/AEM Naming Conventions.md` of the forms
// conversion engine), as its review applied it to the component tree.
const PREFIXES = {
  NumberField: ["NB"],
  DatePicker: ["DATE"],
  Dropdown: ["DD"],
  Checkbox: ["CB"],
  RadioButton: ["RB"],
  TitleDraw: ["TTL"],
  TextDraw: ["ST", "ITXT", "ETXT"],
  HtmlDisplayer: ["TBL", "CRT", "IMG"],
};
const TEXT_FIELD_PREFIXES = {
  Plain: ["TXT"],
  Multiline: ["TXTM"],
  Email: ["EML"],
  Telephone: ["TEL"],
};
const REPEAT_PREFIXES = ["RCP", "RCBP", "RCHP", "RCHT"];
const FIXED_PANEL_NAMES = ["summaryPanel", "previewPanel", "PN_Preview", "guideRootPanel", "FormMetadata"];

// The prefix before the first `_`, or none: a bare `PN` is not `PN_...`.
function prefixOf(name) {
  const cut = name.indexOf("_");
  return cut < 0 ? null : name.slice(0, cut);
}

// The prefixes a node may use, or null when the profile names it.
function allowed(node, inRepeat) {
  const name = node.name;
  if (typeof name !== "string" || name === "") return null;
  if (name.indexOf("affrg") >= 0 || name.startsWith("AF_") || name.toLowerCase() === "preview") {
    return null;
  }
  switch (node.type) {
    case "Panel":
      if (node.frag_ref || FIXED_PANEL_NAMES.indexOf(name) >= 0) return null;
      return inRepeat ? REPEAT_PREFIXES.concat(["PN"]) : ["PN"];
    case "Repeatable":
      return REPEAT_PREFIXES;
    case "TextField":
      return TEXT_FIELD_PREFIXES[node.kind || "Plain"] || null;
    default:
      return PREFIXES[node.type] || null;
  }
}

function check(output, ctx) {
  const violations = [];
  function visit(node, pointer, inRepeat) {
    const valid = allowed(node, inRepeat);
    if (valid && valid.indexOf(prefixOf(node.name)) < 0) {
      violations.push({
        pointer: pointer + "/name",
        message:
          node.type + " `" + node.name + "` must be named `" + valid[0] + "_...`" +
          (valid.length > 1 ? " (or " + valid.slice(1).map(function (p) { return "`" + p + "_`"; }).join(", ") + ")" : ""),
      });
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) {
      visit(children[i], pointer + "/children/" + i, inRepeat || node.type === "Repeatable");
    }
  }
  visit(output.form, "/form", false);
  return { pass: violations.length === 0, violations: violations };
}
