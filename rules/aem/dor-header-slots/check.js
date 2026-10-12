const SLOTS = ["slot1", "slot2", "slot3"];
// The static node types: a master-page line among them is a `TextDraw`.
const STATIC = ["TextDraw", "TitleDraw", "MessageBox", "HtmlDisplayer"];
// What keeps a header line off the page and the summary step but in the printed header.
const FLAGS = [
  ["visible", false],
  ["always_in_pdf", true],
  ["show_if_hidden", true],
  ["summary_exclude", true],
];

function check(output, ctx) {
  const violations = [];
  const slotted = [];
  let preface = false;
  function visit(node, pointer) {
    if (node.type === "Preface") preface = true;
    if (typeof node.dor_header_slot === "string") slotted.push({ node: node, pointer: pointer });
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");

  const drawn = {};
  for (const { node, pointer } of slotted) {
    const slot = node.dor_header_slot;
    const at = pointer + "/dor_header_slot";
    if (SLOTS.indexOf(slot) < 0) {
      violations.push({
        pointer: at,
        message: "`" + node.name + "` prints in the header slot `" + slot + "`, which is none of " +
          SLOTS.join(", ") + ": the legal entity is `slot2` and the validity line `slot3`",
      });
      continue;
    }
    const isStatic = STATIC.indexOf(node.type) >= 0;
    if (slot === "slot1" && (preface || isStatic)) {
      violations.push({
        pointer: at,
        message: "`" + node.name + "` takes `slot1`, which is the banking relationship's: the legal " +
          "entity is `slot2` and the validity line `slot3`",
      });
    }
    // An input field may print its value in a slot; the rest holds for the static lines.
    if (!isStatic) continue;
    if (node.type !== "TextDraw") {
      violations.push({
        pointer: pointer,
        message: "`" + node.name + "` is a " + node.type + " in a header slot: a master-page line is a " +
          "static `TextDraw`, so replace it with one",
      });
      continue;
    }
    if (drawn[slot] !== undefined) {
      violations.push({
        pointer: at,
        message: "`" + node.name + "` is a second static text in `" + slot + "`, beside `" + drawn[slot] +
          "`: one master-page line per slot, so delete one or move it to its own slot",
      });
    } else {
      drawn[slot] = node.name;
    }
    for (const [flag, value] of FLAGS) {
      // A flag left out is its default: shown, and every switch off.
      const actual = node[flag] === undefined ? flag === "visible" : node[flag];
      if (actual !== value) {
        violations.push({
          // A flag left out has no pointer of its own: name the node.
          pointer: node[flag] === undefined ? pointer : pointer + "/" + flag,
          message: "the header line `" + node.name + "` needs `" + flag + ": " + value + "`, so it is " +
            "hidden on the page and off the summary step but printed in the Document of Record's header",
        });
      }
    }
    if (node.dor_exclude === true) {
      violations.push({
        pointer: pointer + "/dor_exclude",
        message: "the header line `" + node.name + "` exists to reach the Document of Record: remove `dor_exclude`",
      });
    }
  }
  return { pass: violations.length === 0, violations: violations };
}
