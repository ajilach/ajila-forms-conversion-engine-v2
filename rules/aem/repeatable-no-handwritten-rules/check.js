// A script model of a rule starts at `"script":{` (the XML carries it as `&quot;script&quot;:{`).
const SEGMENT = /(?:&quot;|")script(?:&quot;|")\s*:\s*\{/;
// `AemNode::UNBOUNDED_OCCUR`, which the writer writes as `maxOccur="-1"`.
const UNBOUNDED = 4294967295;
const UNBOUNDED_MAX = 20;

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
    if (node.type === "Repeatable") {
      const min = node.min_occur, max = node.max_occur;
      if (max === UNBOUNDED || max < 0) {
        violations.push({
          pointer: pointer + "/max_occur",
          message: "`" + node.name + "` is unbounded; an unbounded section takes at most " + UNBOUNDED_MAX +
            " rows: set max_occur to " + UNBOUNDED_MAX,
        });
      } else if (typeof max === "number" && (max === 0 || (typeof min === "number" && max < min))) {
        violations.push({
          pointer: pointer + "/max_occur",
          message: "`" + node.name + "` takes at most " + max + " rows but at least " + min +
            ": set min_occur and max_occur to the source's min and max (ubs-aem-repeatable-bounds)",
        });
      }
    }
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
