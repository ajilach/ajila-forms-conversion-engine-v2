// Writes each reported text again: entity names as the language writes them, each bare address of
// a static text as the link the corpus writes (`<a href="…" target="_blank">…</a>`, `https://`
// before `www.`, `mailto:` before an e-mail), and every string in Unicode NFC. An address the
// check marks as doubled or cut short by a query string is left for the author.
const STATIC = ["TextDraw", "TitleDraw", "MessageBox", "HtmlDisplayer"];
const TAG = /(<\/?[A-Za-z][^>]*>|<!--[\s\S]*?-->)/;
const URL_RE = /(?:https?:\/\/|www\.)[^\s<>"'&;]+/gi;
const MAIL_RE = /[\w.%+-]+@[\w-]+(?:\.[\w-]+)+/g;
const TRAIL = ".,:;!?»”";
const LATIN = ["fr", "it", "es", "sp"];
const SWISS_AG = /\bUBS (?:Schweiz|Suisse|Svizzera|Suiza|Switzerland) (?:AG|SA)\b/g;

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
      const doubled = low.split("www.").length > 2 || low.split("http").length > 2 || low.split("@").length > 2 ||
        (text.length > 16 && text.indexOf(text.slice(0, 8), 1) >= 0);
      const query = (segment.slice(end) + tail)[0] === "&";
      found.push({ start: m.index, end: end, text: text, kind: pair[0], linkable: !doubled && !query });
    }
  });
  return found.sort(function (a, b) { return a.start - b.start; });
}

function link(html) {
  const parts = String(html).split(TAG);
  let anchor = 0;
  for (let i = 0; i < parts.length; i++) {
    if (i % 2 === 1) {
      if (/^<a[\s>]/i.test(parts[i])) anchor++;
      else if (/^<\/a\s*>/i.test(parts[i]) && anchor > 0) anchor--;
      continue;
    }
    if (anchor > 0) continue;
    const found = addresses(parts[i], parts[i + 1] || "").filter(function (a) { return a.linkable; });
    for (let k = found.length - 1; k >= 0; k--) {
      const a = found[k];
      const href = a.kind === "mail" ? "mailto:" + a.text : /^www\./i.test(a.text) ? "https://" + a.text : a.text;
      parts[i] = parts[i].slice(0, a.start) + "<a href=\"" + href + "\" target=\"_blank\">" + a.text + "</a>" +
        parts[i].slice(a.end);
    }
  }
  return parts.join("");
}

function names(html, lang) {
  const parts = String(html).split(TAG);
  const latin = LATIN.indexOf(String(lang).toLowerCase().split("-")[0]) >= 0;
  for (let i = 0; i < parts.length; i += 2) {
    parts[i] = parts[i].replace(SWISS_AG, "UBS Switzerland AG");
    if (latin) parts[i] = parts[i].replace(/\bUBS AG\b/g, "UBS SA").replace(/\bUBS Group AG\b/g, "UBS Group SA");
  }
  return parts.join("");
}

function nfc(value) {
  return typeof value.normalize === "function" ? value.normalize("NFC") : value;
}

function isText(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function fix(output, ctx) {
  const ops = [];
  const done = {};

  function text(map, pointer, rich) {
    if (!isText(map)) return;
    for (const lang of Object.keys(map)) {
      const value = map[lang];
      if (typeof value !== "string") continue;
      let fixed = names(nfc(value), lang);
      if (rich) fixed = link(fixed);
      const at = pointer + "/" + lang;
      done[at] = true;
      if (fixed !== value) ops.push({ op: "replace", path: at, value: fixed });
    }
  }
  function visit(node, pointer) {
    text(node.title, pointer + "/title", false);
    text(node.label, pointer + "/label", false);
    text(node.content, pointer + "/content", STATIC.indexOf(node.type) >= 0);
    const options = node.options || [];
    for (let i = 0; i < options.length; i++) text(options[i].label, pointer + "/options/" + i + "/label", false);
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");

  // Every other string of the document only needs composing.
  walk(output, function (value, pointer) {
    if (typeof value === "string" && !done[pointer] && nfc(value) !== value) {
      ops.push({ op: "replace", path: pointer, value: nfc(value) });
    }
  });
  return ops;
}
