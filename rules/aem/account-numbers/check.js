// The account-number fragments of `afforms_ubs_fragmentlib`, by kind, and the IBAN fragments that
// copy their IBAN into the Document of Record header's banking-relationship (IBAN1) or custody (IBAN2)
// field.
const ACCOUNT_FRAGMENTS = ["affrg_account1", "affrg_custodyaccount1", "affrg_product1",
  "affrg_safekepingaccount1", "affrg_safedepositbox1"];
const IBAN_FRAGMENTS = ["affrg_iban1", "affrg_iban2"];
// One value per form: the custody account and the IBAN of the banking relationship or of the
// custody account are captured once (UBS-ACC-5).
const ONCE = ["affrg_custodyaccount1", "affrg_iban1", "affrg_iban2"];

function fileOf(node) {
  return node.type === "Fragment" && typeof node.frag_ref === "string" ? node.frag_ref.split("/").pop().toLowerCase() : "";
}

function check(output, ctx) {
  const violations = [];
  const seen = {};
  // `chain`: the nodes from the form down to the parent, with their pointers.
  function visit(node, pointer, chain) {
    const file = fileOf(node);
    if (ACCOUNT_FRAGMENTS.indexOf(file) >= 0 || IBAN_FRAGMENTS.indexOf(file) >= 0) {
      // Full width: no panel between the page and the fragment is narrower than the grid.
      for (let i = 1; i < chain.length; i++) {
        const n = chain[i].node;
        if (n.type === "Panel" && typeof n.colspan === "number" && n.colspan > 0 && n.colspan < 12) {
          violations.push({
            pointer: chain[i].pointer + "/colspan",
            message: "`" + n.name + "` is " + n.colspan + " columns wide and holds the account fragment `" + node.name +
              "`: an account number takes the full width, so set colspan 12 (the fields beside it go on rows of their own)",
          });
        }
      }
      // Captured once: the second copy shown under the same conditions, outside any repeating row.
      const inRow = chain.some(function (c) { return c.node.type === "Repeatable"; });
      if (ONCE.indexOf(file) >= 0 && !inRow) {
        const conditions = chain.filter(function (c) { return c.node.is_conditional === true; })
          .map(function (c) { return c.pointer; }).join("|");
        const key = file + "#" + conditions;
        if (seen[key]) {
          violations.push({
            pointer: pointer,
            message: "`" + node.name + "` asks again for the number `" + seen[key] + "` already asks (" +
              node.frag_ref.split("/").pop() + "): a custody account or its IBAN is captured once, where it first " +
              "appears, so delete this copy (a later place that only shows the number gets no second fragment)",
          });
        } else {
          seen[key] = node.name;
        }
      }
    }
    const here = chain.concat([{ node: node, pointer: pointer }]);
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i, here);
  }
  visit(output.form, "/form", []);
  return { pass: violations.length === 0, violations: violations };
}
