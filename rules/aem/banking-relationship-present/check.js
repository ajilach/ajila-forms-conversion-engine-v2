// The panels under the form that are not wizard steps (same list as the step-titles rule).
const SPECIAL_NAMES = ["formmetadata", "formMetadata", "FormMetadata", "metadata", "autoSaveInfo",
  "signerInfo", "doroptionsubs", "PN_Preview"];

function isSpecial(node) {
  const name = typeof node.name === "string" ? node.name : "";
  return SPECIAL_NAMES.indexOf(name) >= 0 || name.toLowerCase() === "preview";
}

function plain(text) {
  return String(text).replace(/<[^>]+>/g, " ").replace(/&nbsp;/g, " ").replace(/\s+/g, " ").trim().toLowerCase();
}

function check(output, ctx) {
  const violations = [];
  const top = output.form.children || [];
  let first = -1;
  for (let i = 0; i < top.length; i++) {
    if (top[i].type === "Panel" && !isSpecial(top[i])) { first = i; break; }
  }
  const firstPointer = first < 0 ? null : "/form/children/" + first;
  let seen = 0;
  function visit(node, pointer) {
    if (node.type === "Preface") {
      seen += 1;
      if (seen > 1) {
        violations.push({
          pointer: pointer,
          message: "a second `Preface`: the form carries the banking relationship once, so delete this one",
        });
      } else if (firstPointer === null || pointer.indexOf(firstPointer + "/") !== 0) {
        violations.push({
          pointer: pointer,
          message: "the banking relationship `Preface` belongs on the first page; move it there",
        });
      }
    }
    if (node.type === "TextDraw" || node.type === "TitleDraw" || node.type === "MessageBox") {
      const content = node.content || {};
      for (const lang of Object.keys(content)) {
        if (plain(content[lang]) === "ubs europe se") {
          violations.push({
            pointer: pointer + "/content/" + lang,
            message: "`" + node.name + "` is the line \"UBS Europe SE\", which the banking relationship fragment " +
              "renders itself: delete the draw (the entity reaches the Document of Record header from `/header`; " +
              "where it heads the bank's signature block, it is that signature Repeatable's `title`)",
          });
        }
      }
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  if (seen === 0) {
    violations.push({
      pointer: "/form",
      message: "the form has no banking relationship: add a `Preface` node (name `PN_BR`) as the first child of the first page",
    });
  }
  return { pass: violations.length === 0, violations: violations };
}
