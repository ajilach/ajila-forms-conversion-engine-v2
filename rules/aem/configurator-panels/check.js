// The band is the panel the writer's `is_configurator_band` recognises; see rule.toml.
const BAND = "PN_FormConfigurator";
const CHOICES = ["RadioButton", "Dropdown"];
// A rule document of a script model starts at `"script":{` (the XML carries it as `&quot;script&quot;:{`).
const SEGMENT = /(?:&quot;|")script(?:&quot;|")\s*:\s*\{/;
const RESET = /resetData\s*\(|resetAllPanelInstances\s*\(/;

function isBand(node) {
  return node.type === "Panel" && typeof node.name === "string" && node.name.indexOf(BAND) === 0;
}

// Whether `text` holds a rule that resets data and is not one the writer generated.
function handWrittenReset(text) {
  if (!RESET.test(text)) return false;
  return text.split(SEGMENT).some(function (segment) {
    return RESET.test(segment) && segment.indexOf("Generated automatically") < 0;
  });
}

// The band's own choices that are not a horizontal RadioButton, as {node, pointer}.
// The band is the first page itself or, when a subtitle needs the first page's title, a Panel
// directly inside it (ubs-aem-form-title).
function crooked(output) {
  const out = [];
  function choices(band, pointer) {
    (band.children || []).forEach(function (child, j) {
      if (CHOICES.indexOf(child.type) < 0) return;
      if (child.type === "RadioButton" && child.alignment === "Horizontal") return;
      out.push({ node: child, pointer: pointer + "/children/" + j });
    });
  }
  const pages = output.form.children || [];
  pages.forEach(function (page, i) {
    const pointer = "/form/children/" + i;
    if (isBand(page)) return choices(page, pointer);
    (page.children || []).forEach(function (child, j) {
      if (isBand(child)) choices(child, pointer + "/children/" + j);
    });
  });
  return out;
}

function check(output, ctx) {
  const violations = [];
  const pages = output.form.children || [];
  let firstPanel = -1;
  for (let i = 0; i < pages.length && firstPanel < 0; i++) if (pages[i].type === "Panel") firstPanel = i;

  let bands = 0;
  function visit(node, pointer, depth, index) {
    if (isBand(node)) {
      bands++;
      // The first page, or a Panel directly inside a first page that is not the band itself.
      const firstPage = firstPanel >= 0 ? pages[firstPanel] : null;
      const first = (depth === 1 && index === firstPanel && node.is_page === true) ||
        (depth === 2 && firstPage !== null && !isBand(firstPage) &&
          pointer.indexOf("/form/children/" + firstPanel + "/children/") === 0);
      if (!first || bands > 1) {
        violations.push({
          pointer: pointer + "/name",
          message: "`" + node.name + "` is " + (bands > 1 ? "a second configurator band" : "not the form's first page") +
            ": the configurator inputs form one band, the first page or a Panel directly inside it (UBS-CFG-2), and the writer resets every choice " +
            "inside a panel so named; move it there, or rename it",
        });
      }
      (node.children || []).forEach(function (child, j) {
        if (CHOICES.indexOf(child.type) < 0) return;
        const at = pointer + "/children/" + j;
        (child.conditions || []).forEach(function (rule, k) {
          if (rule.show === false) {
            violations.push({
              pointer: at + "/conditions/" + k,
              message: "the configurator `" + child.name + "` hides `" + rule.target_panel_name + "` with show: false, which the " +
                "writer ignores, for the visibility and the reset alike: show each option's panel with show: true instead",
            });
          }
        });
      });
    }
    const passthrough = node.passthrough || {};
    const attrs = passthrough.raw_attributes || {};
    Object.keys(attrs).forEach(function (key) {
      if (handWrittenReset(String(attrs[key]))) {
        violations.push({
          pointer: pointer + "/passthrough/raw_attributes/" + key,
          message: "`" + node.name + "` carries a reset of its own (resetData or resetAllPanelInstances); the writer writes " +
            "the configurator's reset (UBS-CFG-4): delete the entry",
        });
      }
    });
    (passthrough.raw_children || []).forEach(function (raw, k) {
      if (handWrittenReset(String(raw))) {
        violations.push({
          pointer: pointer + "/passthrough/raw_children/" + k,
          message: "`" + node.name + "` carries a reset of its own (resetData or resetAllPanelInstances); the writer writes " +
            "the configurator's reset (UBS-CFG-4): delete the element",
        });
      }
    });
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i, depth + 1, i);
  }
  visit(output.form, "/form", 0, 0);

  crooked(output).forEach(function (entry) {
    violations.push({
      pointer: entry.pointer + (entry.node.type === "Dropdown" ? "/type" : "/alignment"),
      message: entry.node.type + " `" + entry.node.name + "` is a configurator choice, which is rendered as horizontal radio " +
        "buttons whatever the source control: make it a RadioButton with alignment Horizontal",
    });
  });
  return { pass: violations.length === 0, violations: violations };
}
