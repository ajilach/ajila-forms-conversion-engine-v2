// A heading renders once, as plain text, at a level that follows the headings before it.
// Mirrors the step-title duplicate of `step_title_dupes.py` (PROBLEM-step-title-single), and adds
// UBS-SEC-2 (no drawn copy of a section heading), UBS-SEC-3 (levels do not skip), the backlog's
// "Rich text in titles" (a panel title shows no markup) and CNV-TXT-8 (no caption repeating the
// label beside it). fix.js shares `analyse` and repairs every finding but a footnote reference in
// a title below page level.
const SUBTITLE_CSS = "subtitle-after-form-title";
const INPUTS = ["TextField", "NumberField", "DatePicker", "Dropdown", "Checkbox", "RadioButton"];
// A footnote reference as the writer emits it: the one markup a page title may keep, since the
// step title draw renders it (it is stripped from the panel's own title property only).
const FOOTNOTE_REF = /<span\b[^>]*data-af-footnote-id[^>]*>\s*<sup>[^<]*<\/sup>\s*<\/span>/gi;
const MARKUP = /<\/?[A-Za-z][^>]*>/g;
const HAS_MARKUP = /<\/?[A-Za-z][^>]*>/;

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

// Markup, footnote references, case, whitespace and a trailing colon do not make a text a
// different one.
function plain(text) {
  return String(text).replace(FOOTNOTE_REF, "").replace(/<p\b[^>]*footnoteDescription[\s\S]*?<\/p>/gi, "")
    .replace(/<[^>]+>/g, " ").replace(/&nbsp;/g, " ").replace(/\s+/g, " ").trim()
    .replace(/\s*:$/, "").toLowerCase();
}

function isDraw(node) {
  return node.type === "TitleDraw" || node.type === "TextDraw";
}

function isSubtitle(node) {
  return typeof node.css === "string" && node.css.split(/\s+/).indexOf(SUBTITLE_CSS) >= 0;
}

// A title as plain text: tags out (a break or a block boundary is a space), whitespace collapsed.
function stripTitle(text, keepFootnotes) {
  const kept = [];
  let out = String(text);
  if (keepFootnotes) {
    out = out.replace(FOOTNOTE_REF, function (ref) {
      kept.push(ref);
      return "\u0000" + (kept.length - 1) + "\u0000";
    });
  }
  out = out.replace(MARKUP, " ").replace(/\s+/g, " ").trim();
  return out.replace(/\s*\u0000([0-9]+)\u0000/g, function (all, i) { return kept[Number(i)]; });
}

// Everything the rule finds, with what repairs it: `remove` (node pointers), `levels` (pointer of a
// `heading_level` to its level), `titles` (pointer of a title text to its plain text) and
// `references` (a footnote reference in a title that cannot keep one).
function analyse(output) {
  const master = masterLanguage(output);
  const found = { violations: [], remove: [], levels: {}, titles: {} };
  const removed = {};
  function drop(pointer, message) {
    if (removed[pointer]) return;
    removed[pointer] = true;
    found.remove.push(pointer);
    found.violations.push({ pointer: pointer, message: message });
  }
  function drawText(node) {
    return plain(masterText(node.content, master));
  }

  // A draw repeating the draw before it, and a caption repeating the label of the input beside it.
  function siblings(node, pointer) {
    const children = node.children || [];
    let kept = null;
    for (let i = 0; i < children.length; i++) {
      const child = children[i];
      const at = pointer + "/children/" + i;
      // The sibling this one follows once the deleted ones are gone.
      const previous = kept;
      const next = i + 1 < children.length ? children[i + 1] : null;
      if (previous && isDraw(child) && isDraw(previous) && !isSubtitle(child) && !isSubtitle(previous)) {
        const text = drawText(child);
        if (text !== "" && text === drawText(previous)) {
          drop(at, "`" + child.name + "` repeats the text of the draw `" + previous.name +
            "` right before it, so the same text renders twice: delete it");
        }
      }
      if (child.type === "TextDraw" && !isSubtitle(child) && !child.dor_header_slot) {
        const text = drawText(child);
        [previous, next].forEach(function (input) {
          if (text !== "" && input && INPUTS.indexOf(input.type) >= 0 && input.hide_title !== true && plain(masterText(input.label, master)) === text) {
            drop(at, "`" + child.name + "` repeats the label of `" + input.name + "` beside it, which the input " +
              "already shows: delete the caption (CNV-TXT-8)");
          }
        });
      }
      if (!removed[at]) kept = child;
      siblings(child, at);
    }
  }
  siblings(output.form, "/form");

  // A drawn copy of a page's title, which the writer already renders as the page's heading.
  function page(panel, pointer) {
    const title = plain(masterText(panel.title, master));
    if (title === "") return;
    function among(parent, at) {
      const children = parent.children || [];
      for (let i = 0; i < children.length; i++) {
        const child = children[i];
        if (isDraw(child) && !isSubtitle(child) && drawText(child) === title) {
          drop(at + "/children/" + i, "`" + child.name + "` repeats the title of page `" + panel.name +
            "`, which the writer already renders as the page's heading: delete the draw");
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

  // Heading levels under a page (level 2) do not skip: each heading is at most one level below the
  // heading it follows. A skipped level is lifted to the one the run of headings reaches.
  function levels(pagePanel, pointer) {
    const stack = [{ original: 2, level: 2 }];
    (function visit(node, at) {
      if (node.type === "TitleDraw" && !removed[at]) {
        const original = typeof node.heading_level === "number" ? node.heading_level : 3;
        let level;
        if (original <= 2) {
          stack.length = 1;
          level = 2;
        } else {
          while (stack.length > 1 && stack[stack.length - 1].original >= original) stack.pop();
          level = stack[stack.length - 1].level + 1;
          stack.push({ original: original, level: level });
        }
        if (level !== original) {
          found.levels[at + "/heading_level"] = level;
          found.violations.push({
            pointer: at + "/heading_level",
            message: original < 2
              ? "`" + node.name + "` is a level-" + original + " heading, the form title's level: a heading in a page is " +
                "level 2 or deeper, make it 2"
              : "`" + node.name + "` is a level-" + original + " heading where the headings before it in the page " +
                "reach level " + (level - 1) + ": levels do not skip, make it " + level + " (UBS-SEC-3)",
          });
        }
      }
      const children = node.children || [];
      for (let i = 0; i < children.length; i++) visit(children[i], at + "/children/" + i);
    })(pagePanel, pointer);
  }

  const top = output.form.children || [];
  for (let i = 0; i < top.length; i++) {
    if (top[i].type === "Panel" && top[i].is_page === true) {
      page(top[i], "/form/children/" + i);
      levels(top[i], "/form/children/" + i);
    }
  }

  // Panel and Repeatable titles are plain text; a page's may keep a footnote reference.
  (function titles(node, pointer, depth) {
    if ((node.type === "Panel" || node.type === "Repeatable") && node.title && typeof node.title === "object") {
      const isPage = node.type === "Panel" && node.is_page === true && depth === 1;
      for (const lang of Object.keys(node.title)) {
        const text = String(node.title[lang]);
        const at = pointer + "/title/" + lang;
        if (!isPage && /data-af-footnote-id/.test(text)) {
          found.violations.push({
            pointer: at,
            message: "`" + node.name + "`'s title (" + lang + ") carries a footnote reference, which only a page title " +
              "keeps: put the reference after the word that carries it in the panel's first text, or in a TitleDraw",
          });
          continue;
        }
        const stripped = stripTitle(text, isPage);
        if (stripped !== text && HAS_MARKUP.test(text.replace(FOOTNOTE_REF, ""))) {
          found.titles[at] = stripped;
          found.violations.push({
            pointer: at,
            message: "`" + node.name + "`'s title (" + lang + ") carries markup, which a panel title cannot show: " +
              "write it as plain text",
          });
        }
      }
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) titles(children[i], pointer + "/children/" + i, depth + 1);
  })(output.form, "/form", 0);

  return found;
}

function check(output, ctx) {
  const violations = analyse(output).violations;
  return { pass: violations.length === 0, violations: violations };
}
