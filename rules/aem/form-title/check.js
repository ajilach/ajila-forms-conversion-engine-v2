const SUBTITLE_CSS = "subtitle-after-form-title";
// The issuer, which is never the subtitle (as ubs-aem-issuer-information reads it).
const ENTITY_LINE = /^\s*(?:per\s+)?ubs\s+(?:europe\s+se|switzerland\s+ag)\b[\s,()\-–]*(?:succursale\s+italia)?[\s,()\-–.]*$/i;
// Dashes and list markers at title size are no title (UBS-TTL-5).
const MARKERS = /^[\s\-–—•·*_.]+$/;
// "At most about 80 characters": past this a line is a paragraph.
const SUBTITLE_MAX = 100;

function plain(text) {
  return String(text).replace(/<[^>]+>/g, " ").replace(/&nbsp;/g, " ").replace(/\s+/g, " ").trim();
}

function tokens(node) {
  return typeof node.css === "string" ? node.css.split(/\s+/).filter(function (t) { return t !== ""; }) : [];
}

// The writer's first page: the first page Panel under the root.
function firstPageIndex(top) {
  for (let i = 0; i < top.length; i++) {
    if (top[i].type === "Panel" && top[i].is_page === true) return i;
  }
  return -1;
}

function check(output, ctx) {
  const violations = [];
  const languages = output.languages || [];
  const form = output.form;
  const title = form.title && typeof form.title === "object" ? form.title : {};

  // UBS-TTL-1: the form title, in every language, as plain text.
  for (const lang of languages) {
    const text = typeof title[lang] === "string" ? title[lang] : "";
    if (plain(text) === "" || MARKERS.test(plain(text))) {
      violations.push({
        pointer: Object.prototype.hasOwnProperty.call(title, lang) ? "/form/title/" + lang : "/form/title",
        message: "the form title is " + (plain(text) === "" ? "empty" : "only list markers") + " in `" + lang +
          "`: write the 18 pt title that language's PDF prints at the top of page 1 (a letter's subject " +
          "line, or else the form code)",
      });
    } else if (/<[a-z\/][^>]*>/i.test(text)) {
      violations.push({
        pointer: "/form/title/" + lang,
        message: "the form title carries markup in `" + lang + "`: the writer prints it as plain text, so write the words only",
      });
    }
  }

  const top = form.children || [];
  const first = firstPageIndex(top);
  const page = first < 0 ? null : top[first];
  const pagePointer = "/form/children/" + first;
  const subtitled = page !== null && !(typeof page.name === "string" && page.name.indexOf("PN_FormConfigurator") === 0);
  const subtitle = {};
  if (subtitled) {
    const map = page.title && typeof page.title === "object" ? page.title : {};
    for (const lang of Object.keys(map)) {
      const text = plain(map[lang]);
      if (text === "") continue;
      subtitle[lang] = text.toLowerCase();
      const at = pagePointer + "/title/" + lang;
      const why = ENTITY_LINE.test(text) ? "is the issuer, which is the page header's `slot2` line"
        : plain(title[lang] || "").toLowerCase() === text.toLowerCase() ? "repeats the form title"
        : text.length > SUBTITLE_MAX ? "is " + text.length + " characters long, a paragraph rather than a subtitle"
        : /[.:;,]$/.test(text) ? "ends in punctuation, as a paragraph or a lead-in does"
        : null;
      if (why !== null) {
        violations.push({
          pointer: at,
          message: "the first page's title prints as the subtitle under the form title, and \"" + text + "\" " + why +
            ": keep only the short line right under the 18 pt title there, and leave the title empty when the " +
            "source has none (a paragraph opens the page's content as a TextDraw instead)",
        });
      }
    }
  }

  function visit(node, pointer, onFirst) {
    if (tokens(node).indexOf(SUBTITLE_CSS) >= 0) {
      violations.push({
        pointer: pointer + "/css",
        message: "`" + node.name + "` carries `" + SUBTITLE_CSS + "`, which the writer gives the first page's title: " +
          "put the subtitle in that page's `title`, delete this node, or drop the class",
      });
    }
    // UBS-TTL-4: a title-area radio row labelled with the subtitle hides its label.
    if (onFirst && node.type === "RadioButton" && node.hide_title !== true) {
      const label = node.label && typeof node.label === "object" ? node.label : {};
      const same = Object.keys(label).some(function (lang) {
        return subtitle[lang] !== undefined && plain(label[lang]).toLowerCase() === subtitle[lang];
      });
      if (same) {
        violations.push({
          pointer: pointer + "/hide_title",
          message: "`" + node.name + "` is labelled with the subtitle, which prints right above it: set " +
            "`hide_title: true` so the line does not show twice",
        });
      }
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) {
      visit(children[i], pointer + "/children/" + i, onFirst || (node === form && i === first));
    }
  }
  visit(form, "/form", false);
  return { pass: violations.length === 0, violations: violations };
}
