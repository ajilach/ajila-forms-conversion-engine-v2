const SUBTITLE_CSS = "subtitle-after-form-title";

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

// Markup, case, whitespace and a trailing colon do not make a heading a different one.
function plain(text) {
  return String(text).replace(/<[^>]+>/g, " ").replace(/&nbsp;/g, " ").replace(/\s+/g, " ").trim()
    .replace(/\s*:$/, "").toLowerCase();
}

function isDraw(node) {
  return node.type === "TitleDraw" || node.type === "TextDraw";
}

function isSubtitle(node) {
  return typeof node.css === "string" && node.css.split(/\s+/).indexOf(SUBTITLE_CSS) >= 0;
}

function check(output, ctx) {
  const violations = [];
  const master = masterLanguage(output);

  function drawText(node) {
    return plain(masterText(node.content, master));
  }

  function consecutive(node, pointer) {
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) {
      const child = children[i];
      const childAt = pointer + "/children/" + i;
      const previous = i > 0 ? children[i - 1] : null;
      if (previous && isDraw(child) && isDraw(previous) && !isSubtitle(child) && !isSubtitle(previous)) {
        const text = drawText(child);
        if (text !== "" && text === drawText(previous)) {
          violations.push({
            pointer: childAt,
            message: "`" + child.name + "` repeats the text of the draw `" + previous.name +
              "` right before it, so the same text renders twice: delete one",
          });
        }
      }
      consecutive(child, childAt);
    }
  }
  consecutive(output.form, "/form");

  function page(panel, pointer) {
    const title = plain(masterText(panel.title, master));
    if (title === "") return;
    function among(parent, at) {
      const children = parent.children || [];
      for (let i = 0; i < children.length; i++) {
        const child = children[i];
        if (isDraw(child) && !isSubtitle(child) && drawText(child) === title) {
          violations.push({
            pointer: at + "/children/" + i,
            message: "`" + child.name + "` repeats the title of page `" + panel.name +
              "`, which the writer already renders as the page's heading: delete the draw",
          });
        }
      }
    }
    among(panel, pointer);
    const children = panel.children || [];
    for (let i = 0; i < children.length; i++) {
      if (children[i].type === "Panel") {
        among(children[i], pointer + "/children/" + i);
        break;
      }
    }
  }
  const top = output.form.children || [];
  for (let i = 0; i < top.length; i++) {
    if (top[i].type === "Panel" && top[i].is_page === true) page(top[i], "/form/children/" + i);
  }
  return { pass: violations.length === 0, violations: violations };
}
