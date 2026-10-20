// Turns every choice among the band's direct children into a horizontal RadioButton. A Dropdown
// carries every field a RadioButton has except `alignment`, so the type and the alignment are all
// that change. The other findings need a person.
const BAND = "PN_FormConfigurator";

function fix(output, ctx) {
  const ops = [];
  (output.form.children || []).forEach(function (page, i) {
    if (page.type !== "Panel" || typeof page.name !== "string" || page.name.indexOf(BAND) !== 0) return;
    (page.children || []).forEach(function (child, j) {
      const at = "/form/children/" + i + "/children/" + j;
      if (child.type === "Dropdown") {
        ops.push({ op: "replace", path: at + "/type", value: "RadioButton" });
        ops.push({ op: "add", path: at + "/alignment", value: "Horizontal" });
      } else if (child.type === "RadioButton" && child.alignment !== "Horizontal") {
        ops.push({ op: "add", path: at + "/alignment", value: "Horizontal" });
      }
    });
  });
  return ops;
}
