// The names the profile fixes for the party and signature blocks, which repeat on purpose.
const FIXED = /^(PN|RCP)_(CPGRP|AHGRP|AHGRP_AR|BOGRP|PAGRP)(_[A-Za-z0-9]+)?$|^(RCP|PN)_(SGN|Sign)_/;
// Nodes the profile names itself.
const PROFILE_NAMED = ["Fragment", "Preface", "FootnotePlaceholder", "Root"];

function check(output, ctx) {
  const violations = [];
  const first = {};
  function visit(node, pointer) {
    const name = node.name;
    if (typeof name === "string" && name !== "" && PROFILE_NAMED.indexOf(node.type) < 0 && !FIXED.test(name)) {
      if (Object.prototype.hasOwnProperty.call(first, name)) {
        violations.push({
          pointer: pointer + "/name",
          message: "`" + name + "` is already the name of the node at " + first[name] +
            ": rename one of them (and the conditions that target it), or delete the duplicate",
        });
      } else {
        first[name] = pointer;
      }
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  return { pass: violations.length === 0, violations: violations };
}
