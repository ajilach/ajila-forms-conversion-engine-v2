const PARTNER_GENERICS = ["affrg_ContractualPartnerGeneric", "affrg_PartnertoPartnerGeneric",
  "affrg_BeneficialOwnerGeneric", "affrg_PowerofAttorneyGeneric"];

function isPartnerGeneric(fragRef) {
  const fragment = String(fragRef).split("/").pop();
  return PARTNER_GENERICS.some(function (stem) { return fragment.indexOf(stem) === 0; });
}

function check(output, ctx) {
  const violations = [];
  function visit(node, pointer, parent) {
    if (node.type === "Fragment" && isPartnerGeneric(node.frag_ref) && (!parent || parent.type !== "Repeatable")) {
      violations.push({
        pointer: pointer,
        message: "`" + node.name + "` is a partner generic (" + String(node.frag_ref).split("/").pop() +
          ") outside a Repeatable: a party is a Repeatable named `RCP_<stem>` wrapping its fragment, so the " +
          "Add and Remove buttons and the signature pairing exist",
      });
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i, node);
  }
  visit(output.form, "/form", null);
  return { pass: violations.length === 0, violations: violations };
}
