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
  const entityDraws = [];
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
    // A DoR header slot draw is the master-page line itself, not a body line.
    const isBody = node.type === "TextDraw" || node.type === "TitleDraw" || node.type === "MessageBox";
    if (isBody && !node.dor_header_slot) {
      const content = node.content || {};
      for (const lang of Object.keys(content)) {
        if (plain(content[lang]) === "ubs europe se") {
          entityDraws.push({
            pointer: pointer + "/content/" + lang,
            message: "`" + node.name + "` is the line \"UBS Europe SE\", which the banking relationship fragment " +
              "renders itself: delete the draw (the entity reaches the Document of Record header as the " +
              "`slot2` draw; where it heads the bank's signature block, it is that signature Repeatable's `title`)",
          });
        }
      }
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  // Without a banking relationship the fragment renders no entity line, so a
  // body line is no duplicate of it.
  if (seen > 0) {
    for (const v of entityDraws) violations.push(v);
  }
  return { pass: violations.length === 0, violations: violations };
}
