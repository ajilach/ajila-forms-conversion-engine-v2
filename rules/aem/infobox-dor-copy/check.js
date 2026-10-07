const INFOBOX = "affrg_italy_infobox";

function check(output, ctx) {
  const violations = [];
  const top = output.form.children || [];
  let lastPage = -1;
  for (let i = 0; i < top.length; i++) {
    if (top[i].type === "Panel" && top[i].is_page === true) lastPage = i;
  }
  function visit(node, pointer, page) {
    if (node.type === "Fragment" && typeof node.frag_ref === "string" &&
        node.frag_ref.indexOf(INFOBOX) >= 0 && node.visible === false) {
      const missing = [];
      if (node.always_in_pdf !== true) missing.push("always_in_pdf: true");
      if (node.summary_exclude !== true) missing.push("summary_exclude: true");
      if (missing.length > 0) {
        violations.push({
          pointer: pointer,
          message:
            "the hidden infobox copy `" + node.name + "` needs " + missing.join(" and ") +
            ", or it prints nowhere in the finished document",
        });
      }
      if (node.dor_exclude === true) {
        violations.push({
          pointer: pointer + "/dor_exclude",
          message: "the hidden infobox copy `" + node.name + "` must not set dor_exclude: it would drop the copy from the PDF again",
        });
      }
      if (page !== lastPage) {
        violations.push({
          pointer: pointer,
          message: "the hidden infobox copy `" + node.name + "` belongs inside the last page of the form, where the finished document prints it",
        });
      }
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) {
      visit(children[i], pointer + "/children/" + i, node === output.form ? i : page);
    }
  }
  visit(output.form, "/form", -2);
  return { pass: violations.length === 0, violations: violations };
}
