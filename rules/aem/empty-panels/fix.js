// Rebuilds the form without what check.js reports: empty panels and repeatables go (with the
// conditions that show them), blank draws go, empty paragraphs and edge breaks are cut, and a node
// directly under `form` moves into a page. A panel is removed as a whole when nothing in it
// renders, so removing what is inside a panel never leaves the panel itself empty behind it.
const DRAWS = ["TextDraw", "TitleDraw", "MessageBox", "HtmlDisplayer"];
const FILLER = /^[._…\-\u2013\u2014]{3,}$/;
const EMPTY_PARAGRAPH = /<p\b[^>]*>(?:\s|&nbsp;|&#160;|&#xa0;|\u00a0|<br\s*\/?>)*<\/p>/gi;
const LEADING_BREAKS = /(<p\b[^>]*>)\s*(?:<br\s*\/?>\s*)+/gi;
const TRAILING_BREAKS = /(?:\s*<br\s*\/?>)+\s*(<\/p>)/gi;

function plain(html) {
  return String(html).replace(/<[^>]*>/g, " ").replace(/&(nbsp|#160|#xa0);/gi, " ").replace(/[\s\u00a0]+/g, "");
}

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

function emptyContainer(node) {
  if ((node.type !== "Panel" && node.type !== "Repeatable") || node.frag_ref) return false;
  return (node.children || []).every(function (c) {
    return emptyContainer(c) || (DRAWS.indexOf(c.type) >= 0 && blankDraw(c));
  });
}

function fix(output, ctx) {
  const form = JSON.parse(JSON.stringify(output.form));
  const removed = {};

  function clean(node) {
    if (DRAWS.indexOf(node.type) >= 0 && node.type !== "HtmlDisplayer" && node.content) {
      for (const lang of Object.keys(node.content)) {
        node.content[lang] = String(node.content[lang])
          .replace(EMPTY_PARAGRAPH, "").replace(LEADING_BREAKS, "$1").replace(TRAILING_BREAKS, "$1");
      }
    }
    if (!Array.isArray(node.children)) return;
    node.children = node.children.filter(function (c) {
      if (emptyContainer(c)) {
        removed[c.name] = true;
        return false;
      }
      return !(DRAWS.indexOf(c.type) >= 0 && blankDraw(c));
    });
    node.children.forEach(clean);
  }
  clean(form);

  // A node directly under the form joins the page before it, or opens the first page.
  const pages = [];
  let pending = [];
  let lastPage = null;
  form.children.forEach(function (node) {
    if (node.type === "Panel") {
      if (lastPage === null && pending.length > 0) {
        // After what the first page must open with: the banking relationship, the letter top
        // and the DoR header lines.
        const children = node.children || [];
        let at = 0;
        while (at < children.length && (children[at].type === "Preface" || children[at].type === "Letterhead" ||
            children[at].dor_header_slot)) at++;
        node.children = children.slice(0, at).concat(pending, children.slice(at));
        pending = [];
      }
      pages.push(node);
      lastPage = node;
    } else if (lastPage === null) {
      pending.push(node);
    } else {
      lastPage.children = (lastPage.children || []).concat([node]);
    }
  });
  form.children = pending.concat(pages);

  // A condition that shows a removed panel shows nothing; drop it unless the name lives on.
  const names = {};
  (function count(node) {
    if (typeof node.name === "string") names[node.name] = true;
    (node.children || []).forEach(count);
  })(form);
  (function prune(node) {
    if (Array.isArray(node.conditions)) {
      node.conditions = node.conditions.filter(function (c) {
        return !(removed[c.target_panel_name] && !names[c.target_panel_name]);
      });
    }
    (node.children || []).forEach(prune);
  })(form);

  return [{ op: "replace", path: "/form", value: form }];
}
