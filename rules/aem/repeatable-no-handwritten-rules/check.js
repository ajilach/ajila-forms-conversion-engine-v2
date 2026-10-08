// A script model of a rule starts at `"script":{` (the XML carries it as `&quot;script&quot;:{`).
const SEGMENT = /(?:&quot;|")script(?:&quot;|")\s*:\s*\{/;

// Whether `text` holds a script that calls `instanceManager` and is not the template's own.
function handWritten(text) {
  if (text.indexOf("instanceManager") < 0) return false;
  const segments = text.split(SEGMENT);
  for (const segment of segments) {
    if (segment.indexOf("instanceManager") < 0) continue;
    if (segment.indexOf("Generated automatically") >= 0 || segment.indexOf("_archetype") >= 0) continue;
    return true;
  }
  return false;
}

function check(output, ctx) {
  const violations = [];
  function visit(node, pointer) {
    const passthrough = node.passthrough || {};
    const attrs = passthrough.raw_attributes || {};
    for (const key of Object.keys(attrs)) {
      if (handWritten(String(attrs[key]))) {
        violations.push({
          pointer: pointer + "/passthrough/raw_attributes/" + key,
          message: "`" + node.name + "` carries a rule that calls instanceManager, which bypasses the UBS add and " +
            "remove routines: delete the entry, the template writes the working rules",
        });
      }
    }
    const raw = passthrough.raw_children || [];
    for (let i = 0; i < raw.length; i++) {
      if (handWritten(String(raw[i]))) {
        violations.push({
          pointer: pointer + "/passthrough/raw_children/" + i,
          message: "`" + node.name + "` carries a rule that calls instanceManager, which bypasses the UBS add and " +
            "remove routines: delete the element, the template writes the working rules",
        });
      }
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  return { pass: violations.length === 0, violations: violations };
}
