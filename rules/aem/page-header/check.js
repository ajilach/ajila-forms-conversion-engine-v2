// The panels under the form that are not wizard steps (same list as the step-titles rule).
const SPECIAL_NAMES = ["formmetadata", "formMetadata", "FormMetadata", "metadata", "autoSaveInfo",
  "signerInfo", "doroptionsubs", "PN_Preview"];

function isSpecial(node) {
  const name = typeof node.name === "string" ? node.name : "";
  return SPECIAL_NAMES.indexOf(name) >= 0 || name.toLowerCase() === "preview";
}

// The page the writer treats as the first one: the first page Panel under the root
// (`first_page` of the writer), or the first step when none is marked yet.
function firstPageIndex(top) {
  for (let i = 0; i < top.length; i++) {
    if (top[i].type === "Panel" && top[i].is_page === true) return i;
  }
  for (let i = 0; i < top.length; i++) {
    if (top[i].type === "Panel" && !isSpecial(top[i])) return i;
  }
  return -1;
}

function isSlotDraw(node) {
  return node.type === "TextDraw" && typeof node.dor_header_slot === "string";
}

function isCustody(node) {
  return node.type === "Fragment" && typeof node.frag_ref === "string" &&
    /affrg_CustodyAccount\d*$/.test(node.frag_ref);
}

function check(output, ctx) {
  const violations = [];
  const top = output.form.children || [];

  // UBS-PAGE-5: only pages sit under the root.
  for (let i = 0; i < top.length; i++) {
    if (top[i].type !== "Panel") {
      violations.push({
        pointer: "/form/children/" + i,
        message: "`" + (top[i].name || top[i].type) + "` is a " + top[i].type + " directly under the form, " +
          "where only pages (Panels) sit: move it into its page (page furniture opens the first page)",
      });
    }
  }

  const first = firstPageIndex(top);
  const firstPointer = first < 0 ? null : "/form/children/" + first;
  const kids = first < 0 ? [] : (top[first].children || []);

  // The page header run: Preface, the letter's Letterhead, the slot draws, the custody account.
  // The slot draws follow the Preface even where the Preface itself is out of place, so that
  // moving it is the one fix reported.
  let at = 0;
  for (let i = 0; i < kids.length; i++) {
    if (kids[i].type === "Preface") { at = i + 1; break; }
  }
  if (at < kids.length && kids[at].type === "Letterhead") at += 1;
  const inRun = [];
  while (at < kids.length && isSlotDraw(kids[at])) { inRun.push(kids[at]); at += 1; }
  let custody = 0;
  while (at < kids.length && isCustody(kids[at])) {
    custody += 1;
    if (custody > 1) {
      violations.push({
        pointer: firstPointer + "/children/" + at,
        message: "`" + kids[at].name + "` is a second custody account in the page header, which shows " +
          "one (UBS-ACC-1): delete it",
      });
    }
    inRun.push(kids[at]);
    at += 1;
  }

  let prefaces = 0;
  function visit(node, pointer) {
    if (node.type === "Preface") {
      prefaces += 1;
      if (prefaces > 1) {
        violations.push({
          pointer: pointer,
          message: "a second `Preface`: the form carries the banking relationship once, so delete this one",
        });
      } else if (firstPointer === null || pointer !== firstPointer + "/children/0") {
        violations.push({
          pointer: pointer,
          message: "the banking relationship `Preface` is the first child of the first page" +
            (first >= 0 ? " (`" + top[first].name + "`)" : "") + ", ahead of everything else on it: move it there",
        });
      }
    }
    if (isSlotDraw(node) && inRun.indexOf(node) < 0) {
      violations.push({
        pointer: pointer,
        message: "the header line `" + node.name + "` belongs to the page header: right after the `Preface` " +
          "on the first page (first on the page without one, after the `Letterhead` in a letter), where " +
          "the writer puts it inside the banking relationship's wrapper; move it there",
      });
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  return { pass: violations.length === 0, violations: violations };
}
