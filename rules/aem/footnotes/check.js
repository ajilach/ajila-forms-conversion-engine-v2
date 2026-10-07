const REFERENCE = "data-af-footnote-id";
const ACCORDION = "ubsAccordionFootnote";

function check(output, ctx) {
  const violations = [];
  const references = []; // pointers of the strings that carry a footnote reference
  const placeholders = []; // pointers of the placeholders
  let accordion = false;
  // The whole document: a reference in the header counts as much as one in the form.
  walk(output, function (node, pointer) {
    if (typeof node === "string" && node.indexOf(REFERENCE) >= 0) references.push(pointer);
    if (node !== null && typeof node === "object") {
      if (node.type === "FootnotePlaceholder") placeholders.push(pointer);
      if (typeof node.css === "string" && node.css.split(/\s+/).indexOf(ACCORDION) >= 0) accordion = true;
    }
  });
  // A form still on accordion footnotes is the guard's other branch, which this rule does not
  // model (see rule.toml): leave it alone rather than ask for a placeholder it does not need.
  if (accordion) return { pass: true, violations: violations };
  if (references.length > 0 && placeholders.length === 0) {
    references.forEach(function (pointer) {
      violations.push({
        pointer: pointer,
        message:
          "this text carries a footnote reference but the form has no FootnotePlaceholder node: " +
          "add one to the page that holds the references",
      });
    });
  }
  if (references.length === 0) {
    placeholders.forEach(function (pointer) {
      violations.push({
        pointer: pointer,
        message: "the form has a FootnotePlaceholder but no text carries a footnote reference (data-af-footnote-id): delete the node",
      });
    });
  }
  return { pass: violations.length === 0, violations: violations };
}
