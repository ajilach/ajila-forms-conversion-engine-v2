// Three conventions every text keeps, each settled without the source: Unicode NFC (CNV-TXT-3),
// a web or e-mail address in static text is a link (UBS-TXT-1, as `static_text_links.py` of the
// feedback repo), and UBS entity names per language (UBS-TXT-3). fix.js repairs all three, except
// an address the source doubled or that carries a query string.
const STATIC = ["TextDraw", "TitleDraw", "MessageBox", "HtmlDisplayer"];
const TAG = /(<\/?[A-Za-z][^>]*>|<!--[\s\S]*?-->)/;
const URL_RE = /(?:https?:\/\/|www\.)[^\s<>"'&;]+/gi;
const MAIL_RE = /[\w.%+-]+@[\w-]+(?:\.[\w-]+)+/g;
const TRAIL = ".,:;!?»”";
const LATIN = ["fr", "it", "es", "sp"];
const SWISS_AG = /\bUBS (?:Schweiz|Suisse|Svizzera|Suiza|Switzerland) (?:AG|SA)\b/g;

// The bare addresses of a text segment: [{start, end, text, kind, why}], `why` set when the address
// must not be linked as it stands.
function addresses(segment, tail) {
  const found = [];
  [["url", URL_RE], ["mail", MAIL_RE]].forEach(function (pair) {
    const re = new RegExp(pair[1].source, pair[1].flags);
    let m;
    while ((m = re.exec(segment)) !== null) {
      const before = m.index > 0 ? segment[m.index - 1] : "";
      if (/[\w@.\/%+-]/.test(before)) continue;
      if (found.some(function (f) { return m.index >= f.start && m.index < f.end; })) continue;
      let text = m[0];
      while (text && TRAIL.indexOf(text[text.length - 1]) >= 0) text = text.slice(0, -1);
      while (text.endsWith(")") && text.split(")").length > text.split("(").length) text = text.slice(0, -1);
      if (text === "" || (pair[0] === "url" && /^www\.?$/i.test(text))) continue;
      const end = m.index + text.length;
      const low = text.toLowerCase();
      let why = null;
      if (low.split("www.").length > 2 || low.split("http").length > 2 || low.split("@").length > 2 ||
          (text.length > 16 && text.indexOf(text.slice(0, 8), 1) >= 0)) {
        why = "is written twice over (the link text ran into its target): write it once, then link it";
      } else if ((segment.slice(end) + tail).indexOf("&amp;") === 0 || (segment.slice(end) + tail)[0] === "&") {
        why = "carries a query string the text does not show whole: write the `<a href>` by hand";
      }
      found.push({ start: m.index, end: end, text: text, kind: pair[0], why: why });
    }
  });
  return found.sort(function (a, b) { return a.start - b.start; });
}

// Every bare address of a rich text, outside `<a>` and outside tags.
function bareAddresses(html) {
  const parts = String(html).split(TAG);
  let anchor = 0;
  const out = [];
  for (let i = 0; i < parts.length; i++) {
    if (i % 2 === 1) {
      if (/^<a[\s>]/i.test(parts[i])) anchor++;
      else if (/^<\/a\s*>/i.test(parts[i]) && anchor > 0) anchor--;
      continue;
    }
    if (anchor === 0) addresses(parts[i], parts[i + 1] || "").forEach(function (a) { out.push(a); });
  }
  return out;
}

// The entity names a text writes wrongly for its language.
function wrongNames(html, lang) {
  const text = String(html).split(TAG).filter(function (_, i) { return i % 2 === 0; }).join("");
  const found = [];
  (text.match(SWISS_AG) || []).forEach(function (n) {
    if (n !== "UBS Switzerland AG") found.push("\"" + n + "\" is \"UBS Switzerland AG\" in every language");
  });
  if (LATIN.indexOf(String(lang).toLowerCase().split("-")[0]) >= 0) {
    if (/\bUBS AG\b/.test(text)) found.push("\"UBS AG\" is \"UBS SA\" in this language");
    if (/\bUBS Group AG\b/.test(text)) found.push("\"UBS Group AG\" is \"UBS Group SA\" in this language");
  }
  return found;
}

function isText(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function check(output, ctx) {
  const violations = [];
  if (typeof "".normalize === "function") {
    walk(output, function (value, pointer) {
      if (typeof value === "string" && value.normalize("NFC") !== value) {
        violations.push({
          pointer: pointer,
          message: "this text is not in Unicode NFC (a letter and a separate combining mark stand for one " +
            "character, as `u` + `¨` for `ü`): write each as one character",
        });
      }
    });
  }

  function text(map, pointer, what, rich) {
    if (!isText(map)) return;
    for (const lang of Object.keys(map)) {
      if (typeof map[lang] !== "string") continue;
      const say = [];
      wrongNames(map[lang], lang).forEach(function (n) { say.push(n); });
      if (rich) {
        bareAddresses(map[lang]).forEach(function (a) {
          say.push("`" + a.text + "` " + (a.why || "is not a link: write it as `<a href=\"" +
            (a.kind === "mail" ? "mailto:" : /^www\./i.test(a.text) ? "https://" : "") + a.text +
            "\" target=\"_blank\">" + a.text + "</a>`"));
        });
      }
      if (say.length > 0) violations.push({ pointer: pointer + "/" + lang, message: what + " (" + lang + "): " + say.join("; ") });
    }
  }

  function visit(node, pointer) {
    const what = "`" + (node.name || node.type) + "`";
    text(node.title, pointer + "/title", what + "'s title", false);
    text(node.label, pointer + "/label", what + "'s label", false);
    text(node.content, pointer + "/content", what, STATIC.indexOf(node.type) >= 0);
    const options = node.options || [];
    for (let i = 0; i < options.length; i++) {
      text(options[i].label, pointer + "/options/" + i + "/label", what + "'s option " + (i + 1), false);
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  return { pass: violations.length === 0, violations: violations };
}
