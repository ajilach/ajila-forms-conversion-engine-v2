// Gives an unbounded repeatable the 20 rows an unbounded section takes. The other findings need a
// person: the source's bounds, or a passthrough entry to delete.
const UNBOUNDED = 4294967295;

function fix(output, ctx) {
  const ops = [];
  function visit(node, pointer) {
    if (node.type === "Repeatable" && (node.max_occur === UNBOUNDED || node.max_occur < 0)) {
      ops.push({ op: "replace", path: pointer + "/max_occur", value: 20 });
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  return ops;
}
