// Nothing empty renders, and nothing sits outside a page: CNV-STR-1 (empty panels), UBS-SEC-5 (no
// content directly under the form), and the empty half of CNV-TXT-5 and CNV-SRC-5 (empty
// paragraphs, spacer breaks, draws with no text or only filler). fix.js repairs all of it.
const DRAWS = ["TextDraw", "TitleDraw", "MessageBox", "HtmlDisplayer"];
// A run of at least three filler characters and nothing else is a line to write on, not text.
const FILLER = /^[._…\-\u2013\u2014]{3,}$/;
const EMPTY_PARAGRAPH = /<p\b[^>]*>(?:\s|&nbsp;|&#160;|&#xa0;|\u00a0|<br\s*\/?>)*<\/p>/i;
const EDGE_BREAK = /<p\b[^>]*>\s*<br\s*\/?>|<br\s*\/?>\s*<\/p>/i;

function plain(html) {
  return String(html).replace(/<[^>]*>/g, " ").replace(/&(nbsp|#160|#xa0);/gi, " ").replace(/[\s\u00a0]+/g, "");
}

// A draw that renders nothing in any language: no text, or only filler. An image or a chart is
// content even without text.
function blankDraw(node) {
  const content = node.content || {};
  return Object.keys(content).every(function (lang) {
    const html = String(content[lang]);
    if (/<(img|svg)[\s>]/i.test(html)) return false;
    if (node.type === "HtmlDisplayer") return html.trim() === "";
    const text = plain(html);
    return text === "" || FILLER.test(text);
  });
}

// A Panel or Repeatable whose whole subtree renders nothing. A panel carrying a fragment's `frag_ref`
// is the fragment's, whatever it shows in the document.
function emptyContainer(node) {
  if ((node.type !== "Panel" && node.type !== "Repeatable") || node.frag_ref) return false;
  return (node.children || []).every(function (c) {
    return emptyContainer(c) || (DRAWS.indexOf(c.type) >= 0 && blankDraw(c));
  });
}

function check(output, ctx) {
  const violations = [];
  function visit(node, pointer) {
    const who = "`" + (node.name || node.type) + "`";
    if (emptyContainer(node)) {
      violations.push({
        pointer: pointer,
        message: node.type + " " + who + " holds nothing that renders: remove it, with any condition that shows it",
      });
      return;
    }
    if (DRAWS.indexOf(node.type) >= 0) {
      if (blankDraw(node)) {
        violations.push({ pointer: pointer, message: who + " has no text in any language (or only filler): remove it" });
        return;
      }
      const content = node.content || {};
      if (node.type !== "HtmlDisplayer") {
        for (const lang of Object.keys(content)) {
          const html = String(content[lang]);
          if (EMPTY_PARAGRAPH.test(html) || EDGE_BREAK.test(html)) {
            violations.push({
              pointer: pointer + "/content/" + lang,
              message: who + " (" + lang + ") has an empty paragraph or a line break at a paragraph's edge, " +
                "which prints as a blank line: remove it",
            });
          }
        }
      }
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  const top = output.form.children || [];
  for (let i = 0; i < top.length; i++) {
    if (top[i].type !== "Panel") {
      violations.push({
        pointer: "/form/children/" + i,
        message: "`" + (top[i].name || top[i].type) + "` sits directly under `form`, outside any page: move it " +
          "to the end of the page before it (the start of the first page if none is)",
      });
    }
  }
  visit(output.form, "/form");
  return { pass: violations.length === 0, violations: violations };
}
