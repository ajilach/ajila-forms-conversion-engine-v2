const ALLOWED_TAGS = ["p", "strong", "em", "sup", "sub", "u", "h1", "h2", "h3", "h4", "h5", "h6", "ul", "ol",
  "li", "table", "thead", "tbody", "tr", "td", "th", "a", "span", "br", "img", "div"];
const VOID_TAGS = ["br", "img"];

function problems(html) {
  const found = [];
  const report = function (message) { if (found.indexOf(message) < 0) found.push(message); };
  const stack = [];
  const tag = /<(\/?)([A-Za-z][A-Za-z0-9]*)([^>]*)>/g;
  let m;
  while ((m = tag.exec(html)) !== null) {
    const closing = m[1] === "/";
    const name = m[2].toLowerCase();
    const rest = m[3];
    if (ALLOWED_TAGS.indexOf(name) < 0) {
      report("the tag <" + name + "> is not part of the Quill vocabulary");
      continue;
    }
    if (VOID_TAGS.indexOf(name) >= 0) continue;
    if (rest.replace(/\s+$/, "").endsWith("/")) continue;
    if (closing) {
      if (stack.length === 0) {
        report("a closing </" + name + "> with nothing open");
      } else {
        const open = stack.pop();
        if (open !== name) report("expected </" + open + "> but found </" + name + ">");
      }
    } else {
      stack.push(name);
    }
  }
  if (stack.length > 0) report("unclosed tag(s): " + stack.join(", "));
  const img = /<img\b([^>]*)>/gi;
  while ((m = img.exec(html)) !== null) {
    const alt = /\balt\s*=\s*"([^"]*)"/i.exec(m[1]);
    if (!alt || alt[1].trim() === "") report("an <img> without a non-blank alt attribute");
  }
  return found;
}

function check(output, ctx) {
  const violations = [];
  const assets = output.assets || [];
  for (let i = 0; i < assets.length; i++) {
    if (assets[i].kind !== "text") continue;
    const content = assets[i].content || {};
    for (const lang of Object.keys(content)) {
      for (const message of problems(String(content[lang]))) {
        violations.push({
          pointer: "/assets/" + i + "/content/" + lang,
          message: "asset `" + assets[i].key + "` (" + lang + "): " + message,
        });
      }
    }
  }
  return { pass: violations.length === 0, violations: violations };
}
