const CANONICAL = "/content/forms/af/afforms_ubs_fragmentlib/affrg_BankingRelationship1";
const MARGIN = "ubs-margin-20";

function check(output, ctx) {
  const violations = [];
  function visit(node, pointer) {
    const ref = node.frag_ref;
    if (typeof ref === "string" && ref.indexOf("BankingRelationship") >= 0 &&
        ref.indexOf("CustodyAccount") < 0 && ref !== CANONICAL) {
      violations.push({
        pointer: pointer + "/frag_ref",
        message:
          "`" + node.name + "` references " + ref + ", which is not the banking relationship fragment " +
          CANONICAL + "; use a `Preface` node on the first page, or set frag_ref to that path",
      });
    }
    // A `Preface` named `PN_BR` is fine: its template writes the class itself.
    if (node.name === "PN_BR" && node.type !== "Preface" && node.css !== MARGIN) {
      violations.push({
        pointer: Object.prototype.hasOwnProperty.call(node, "css") && node.css !== null
          ? pointer + "/css" : pointer,
        message:
          "`PN_BR` must have css exactly `" + MARGIN + "` (no other classes); set its css to that, or " +
          "replace the hand-built wrapper with a `Preface` node, which writes it",
      });
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  return { pass: violations.length === 0, violations: violations };
}
