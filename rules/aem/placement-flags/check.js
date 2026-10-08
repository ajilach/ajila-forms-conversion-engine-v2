function check(output, ctx) {
  const violations = [];
  function visit(node, pointer) {
    if (node.always_in_pdf === true && node.dor_exclude === true) {
      violations.push({
        pointer: pointer + "/dor_exclude",
        message: "`" + node.name + "` carries always_in_pdf but also dor_exclude, which drops it from the printed " +
          "document again: delete dor_exclude and keep always_in_pdf with summary_exclude",
      });
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  return { pass: violations.length === 0, violations: violations };
}
