// A node with `always_in_pdf` and `dor_exclude` gets the PDF-only shape: `dor_exclude` deleted and
// `summary_exclude` set, so it stays out of the summary as `dor_exclude` kept it, and prints.
function fix(output, ctx) {
  const ops = [];
  function visit(node, pointer) {
    if (node.always_in_pdf === true && node.dor_exclude === true) {
      ops.push({ op: "remove", path: pointer + "/dor_exclude" });
      if (node.summary_exclude !== true) ops.push({ op: "add", path: pointer + "/summary_exclude", value: true });
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  return ops;
}
