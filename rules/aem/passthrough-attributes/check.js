// The node types whose template does not write `dorExclusion` / `summaryExclusion` itself, so a
// raw attribute of that name reaches the package. Every other template owns both and writes them
// from the node's typed flags, ignoring a raw one.
const RAW_EXCLUSION_TYPES = ["FootnotePlaceholder"];

// The open tags of a serialized fragment, quote-aware.
function tagsOf(xml) {
  return String(xml).match(/<[\w:.\-]+(?:\s+[\w:.\-]+="[^"]*")*\s*\/?>/g) || [];
}

function attributeOf(tag, name) {
  const m = new RegExp("\\s" + name.replace(/[:.\-]/g, "\\$&") + "=\"([^\"]*)\"").exec(tag);
  return m ? m[1] : null;
}

function check(output, ctx) {
  const violations = [];
  function visit(node, pointer) {
    const passthrough = node.passthrough || {};
    const attrs = passthrough.raw_attributes || {};
    const base = pointer + "/passthrough/raw_attributes/";
    if (attrs["excludeFromDoRIfHidden"] === "true") {
      violations.push({
        pointer: base + "excludeFromDoRIfHidden",
        message: "`" + node.name + "` carries excludeFromDoRIfHidden=\"true\"; delete the attribute",
      });
    }
    if (attrs["defaultToCurrentDate"] === "true") {
      violations.push({
        pointer: base + "defaultToCurrentDate",
        message: "`" + node.name + "` carries defaultToCurrentDate=\"true\"; delete the attribute",
      });
    }
    if (RAW_EXCLUSION_TYPES.indexOf(node.type) >= 0 && attrs["dorExclusion"] === "true" &&
        attrs["summaryExclusion"] !== "true") {
      violations.push({
        pointer: base + "dorExclusion",
        message:
          "`" + node.name + "` carries dorExclusion=\"true\" without summaryExclusion=\"true\"; " +
          "add summaryExclusion=\"true\" next to it, or drop the attribute",
      });
    }
    const raw = passthrough.raw_children || [];
    for (let i = 0; i < raw.length; i++) {
      const at = pointer + "/passthrough/raw_children/" + i;
      tagsOf(raw[i]).forEach(function (tag) {
        if (attributeOf(tag, "excludeFromDoRIfHidden") === "true") {
          violations.push({ pointer: at, message: "`" + node.name + "` carries an element with excludeFromDoRIfHidden=\"true\"; delete the attribute" });
        }
        if (attributeOf(tag, "defaultToCurrentDate") === "true") {
          violations.push({ pointer: at, message: "`" + node.name + "` carries an element with defaultToCurrentDate=\"true\"; delete the attribute" });
        }
        if (attributeOf(tag, "dorExclusion") === "true" && attributeOf(tag, "summaryExclusion") !== "true") {
          violations.push({ pointer: at, message: "`" + node.name + "` carries an element with dorExclusion=\"true\" and no summaryExclusion=\"true\"; add it" });
        }
        if (attributeOf(tag, "sling:resourceType") === "fd/af/components/panel") {
          violations.push({ pointer: at, message: "`" + node.name + "` carries an element typed as the default AEM panel (fd/af/components/panel); every panel is the UBS panel, so express it as a Panel node" });
        }
        if (/^<fd:scripts[\s\/>]/.test(tag)) {
          const visible = attributeOf(tag, "fd:visible");
          const init = attributeOf(tag, "fd:init");
          if (visible && visible.indexOf("script") >= 0 && (!init || init.indexOf("script") < 0)) {
            violations.push({ pointer: at, message: "`" + node.name + "` has a visibility rule (fd:visible) but no Initialize rule (fd:init) running the same condition when the form opens; add one, or express the condition as the node's `conditions`" });
          }
        }
      });
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  return { pass: violations.length === 0, violations: violations };
}
