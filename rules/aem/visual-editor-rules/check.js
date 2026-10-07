// The rule properties AEM's visual editor writes.
const PROPERTIES = [
  "fd:visible", "fd:enabled", "fd:click", "fd:valueCommit", "fd:init", "fd:calc", "fd:validate",
  "fd:navigationChange",
];

// Every `fd:rules` start tag in a serialized passthrough child.
function rulesTags(xml) {
  const tags = [];
  let at = xml.indexOf("<fd:rules");
  while (at >= 0) {
    const end = xml.indexOf(">", at);
    tags.push(end < 0 ? xml.slice(at) : xml.slice(at, end));
    at = xml.indexOf("<fd:rules", at + 1);
  }
  return tags;
}

function check(output, ctx) {
  const violations = [];
  function visit(node, pointer) {
    const raw = (node.passthrough && node.passthrough.raw_children) || [];
    for (let i = 0; i < raw.length; i++) {
      const tags = rulesTags(String(raw[i]));
      for (const prop of PROPERTIES) {
        if (tags.some(function (tag) { return tag.indexOf(" " + prop + "=\"") >= 0; })) {
          violations.push({
            pointer: pointer + "/passthrough/raw_children/" + i,
            message: "`" + node.name + "` carries a visual-editor rule in " + prop + " on fd:rules",
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
