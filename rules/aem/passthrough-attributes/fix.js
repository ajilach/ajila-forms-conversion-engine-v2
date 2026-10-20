// Deletes the retired attributes: `excludeFromDoRIfHidden="true"` and `defaultToCurrentDate="true"`
// wherever the passthrough holds them, and a `dorExclusion="true"` that has no
// `summaryExclusion="true"` beside it. A default-panel element or a visibility rule without its
// Initialize twin has to be re-expressed as typed nodes and fields, which a person does.
const RAW_EXCLUSION_TYPES = ["FootnotePlaceholder"];
const TAG = /<[\w:.\-]+(?:\s+[\w:.\-]+="[^"]*")*\s*\/?>/g;

function attributeOf(tag, name) {
  const m = new RegExp("\\s" + name.replace(/[:.\-]/g, "\\$&") + "=\"([^\"]*)\"").exec(tag);
  return m ? m[1] : null;
}

function cleanTag(tag) {
  let out = tag.replace(/\s+excludeFromDoRIfHidden="true"/g, "").replace(/\s+defaultToCurrentDate="true"/g, "");
  if (attributeOf(out, "dorExclusion") === "true" && attributeOf(out, "summaryExclusion") !== "true") {
    out = out.replace(/\s+dorExclusion="true"/g, "");
  }
  return out;
}

function fix(output, ctx) {
  const ops = [];
  function visit(node, pointer) {
    const passthrough = node.passthrough || {};
    const attrs = passthrough.raw_attributes || {};
    const base = pointer + "/passthrough/raw_attributes/";
    if (attrs["excludeFromDoRIfHidden"] === "true") ops.push({ op: "remove", path: base + "excludeFromDoRIfHidden" });
    if (attrs["defaultToCurrentDate"] === "true") ops.push({ op: "remove", path: base + "defaultToCurrentDate" });
    if (RAW_EXCLUSION_TYPES.indexOf(node.type) >= 0 && attrs["dorExclusion"] === "true" && attrs["summaryExclusion"] !== "true") {
      ops.push({ op: "remove", path: base + "dorExclusion" });
    }
    const raw = passthrough.raw_children || [];
    for (let i = 0; i < raw.length; i++) {
      const cleaned = String(raw[i]).replace(TAG, cleanTag);
      if (cleaned !== raw[i]) ops.push({ op: "replace", path: pointer + "/passthrough/raw_children/" + i, value: cleaned });
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  return ops;
}
