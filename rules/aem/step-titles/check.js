const SUBTITLE_CSS = "subtitle-after-form-title";
const STEP_TITLE_CSS = "stepTitle";
// The panels under the root that are not wizard steps, as the guard names them.
const SPECIAL_NAMES = ["formmetadata", "formMetadata", "FormMetadata", "metadata", "autoSaveInfo",
  "signerInfo", "doroptionsubs", "PN_Preview"];

function isSpecial(node) {
  const name = typeof node.name === "string" ? node.name : "";
  return SPECIAL_NAMES.indexOf(name) >= 0 || name.toLowerCase() === "preview";
}

function masterLanguage(output) {
  const languages = output.languages || [];
  return languages.indexOf("en") >= 0 ? "en" : languages[0];
}

function masterText(texts, master) {
  const map = texts || {};
  if (Object.prototype.hasOwnProperty.call(map, master)) return String(map[master]);
  const keys = Object.keys(map).sort();
  return keys.length > 0 ? String(map[keys[0]]) : "";
}

function plain(text) {
  return String(text).replace(/<[^>]+>/g, "").replace(/\s+/g, " ").trim();
}

function tokens(node) {
  return typeof node.css === "string" ? node.css.split(/\s+/).filter(function (t) { return t !== ""; }) : [];
}

// Every node below `page`, depth first in document order, with its parent and pointer.
function below(page, pointer) {
  const out = [];
  function visit(node, at, parent) {
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) {
      const child = children[i];
      const childAt = at + "/children/" + i;
      out.push({ node: child, pointer: childAt, parent: node });
      visit(child, childAt, node);
    }
  }
  visit(page, pointer, null);
  return out;
}

function check(output, ctx) {
  const violations = [];

  // A step's title is the first level-2 heading it renders, and only that one carries the
  // `stepTitle` marker. A titled page's first heading is the writer's own, so every authored
  // marker there is a second one; an untitled page's first heading is its first level-2
  // `TitleDraw`.
  function markers(page, pointer, titled) {
    const inside = below(page, pointer);
    const marked = inside.filter(function (e) { return tokens(e.node).indexOf(STEP_TITLE_CSS) >= 0; });
    if (marked.length === 0) return;
    const first = titled ? null : inside.find(function (e) {
      return e.node.type === "TitleDraw" && e.node.heading_level === 2;
    });
    for (const e of marked) {
      if (first && e.node === first.node) continue;
      violations.push({
        pointer: e.pointer + "/css",
        message: titled
          ? "`" + e.node.name + "` carries the css class `" + STEP_TITLE_CSS + "`, but page `" + page.name +
            "` has a title, whose heading the writer marks itself: remove the class here"
          : "`" + e.node.name + "` carries the css class `" + STEP_TITLE_CSS + "`, but the step title of page `" +
            page.name + "` is its first level-2 heading" + (first ? " `" + first.node.name + "`" : "") +
            ": only that heading carries the class, so move it there",
      });
    }
  }
  const master = masterLanguage(output);
  const hasHeader = typeof output.header === "string" && output.header.trim() !== "";

  const top = output.form.children || [];
  for (let i = 0; i < top.length; i++) {
    const page = top[i];
    if (page.type !== "Panel" || isSpecial(page)) continue;
    const pointer = "/form/children/" + i;
    const titled = plain(masterText(page.title, master)) !== "";
    markers(page, pointer, titled);
    if (!page.is_page) {
      // A step with no title and no heading has nothing to render as one.
      const headed = below(page, pointer).some(function (e) { return e.node.type === "TitleDraw"; });
      if (!titled && !headed) continue;
      violations.push({
        pointer: pointer + "/is_page",
        message:
          "`" + page.name + "` sits directly under the form, so it is a wizard step: set is_page to true so " +
          "the writer builds its step title",
      });
      continue;
    }
    if (titled) continue;

    const inside = below(page, pointer);
    // The first page's subtitle is a static text; a page holding one has no h2 by design.
    if (inside.some(function (e) { return e.node.type !== "TitleDraw" && tokens(e.node).indexOf(SUBTITLE_CSS) >= 0; })) continue;

    const h2 = inside.filter(function (e) { return e.node.type === "TitleDraw" && e.node.heading_level === 2; });
    // A heading is the step title already when it is alone in its panel (the writer's own
    // step-title panel is such a panel) or its panel is named `...Title`.
    const wrapped = h2.some(function (e) {
      const parent = e.parent;
      if (parent.type === "Repeatable") return true;
      return (typeof parent.name === "string" && parent.name.endsWith("Title")) ||
        (parent.children || []).length === 1;
    });
    if (h2.length > 0 && !wrapped) {
      violations.push({
        pointer: h2[0].pointer,
        message:
          "page `" + page.name + "` has no title but opens a level-2 heading `" + h2[0].node.name +
          "` among other content: that heading is the step title in the wrong place. Set the page's title to its text and delete the draw",
      });
      continue;
    }
    if (h2.length === 0) {
      // The first drawn element with text, in document order.
      const first = inside.find(function (e) {
        if (e.node.type === "Preface") return hasHeader;
        if (e.node.type !== "TextDraw" && e.node.type !== "TitleDraw") return false;
        return plain(masterText(e.node.content, master)) !== "";
      });
      if (first && first.node.type === "TitleDraw") {
        violations.push({
          pointer: first.pointer,
          message:
            "page `" + page.name + "` has no title and starts with the heading `" + first.node.name +
            "`: that is the step title written as a draw. Set the page's title to its text and delete the draw",
        });
      }
    }
  }

  // A marker anywhere but under a step.
  function stray(node, pointer) {
    if (tokens(node).indexOf(STEP_TITLE_CSS) >= 0) {
      violations.push({
        pointer: pointer + "/css",
        message:
          "`" + node.name + "` carries the css class `" + STEP_TITLE_CSS + "` outside a wizard step; remove it",
      });
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) stray(children[i], pointer + "/children/" + i);
  }
  for (let i = 0; i < top.length; i++) {
    const node = top[i];
    if (node.type === "Panel" && !isSpecial(node)) continue;
    stray(node, "/form/children/" + i);
  }
  return { pass: violations.length === 0, violations: violations };
}
