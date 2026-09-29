const GLOBAL = "afforms_global_fragmentlib/affrg_global_InternalBankUse_Text_OURef_Signature";

function isInternalBankUse(fragRef) {
  const lower = fragRef.toLowerCase();
  return lower.indexOf("internalbankuse") >= 0 || lower.indexOf("internal_bank_use") >= 0;
}

function check(output, ctx) {
  const violations = [];
  function visit(node, pointer) {
    if (typeof node.frag_ref === "string" && isInternalBankUse(node.frag_ref) &&
        node.frag_ref.indexOf(GLOBAL) < 0) {
      violations.push({
        pointer: pointer + "/frag_ref",
        message: "`" + node.name + "` references " + node.frag_ref +
          "; the internal-bank-use block is /content/dam/formsanddocuments/" + GLOBAL,
      });
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  return { pass: violations.length === 0, violations: violations };
}
