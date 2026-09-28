const MARKET_LIBRARIES = ["afforms_germany_fragmentlib/", "afforms_italy_fragmentlib/"];
const KEPT_FAMILIES = ["internalbankuse", "InternalBankUse", "internal_bank_use", "footnote", "infobox", "BankingRelationship", "FormConfig"];

function retired(fragRef) {
  return MARKET_LIBRARIES.some(function (l) { return fragRef.indexOf(l) >= 0; }) &&
    !KEPT_FAMILIES.some(function (f) { return fragRef.indexOf(f) >= 0; });
}

function check(output, ctx) {
  const violations = [];
  function visit(node, pointer) {
    if (typeof node.frag_ref === "string" && retired(node.frag_ref)) {
      violations.push({
        pointer: pointer + "/frag_ref",
        message: "`" + node.name + "` references the retired market fragment " + node.frag_ref +
          "; use the matching UBS generic from afforms_ubs_fragmentlib",
      });
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  return { pass: violations.length === 0, violations: violations };
}
