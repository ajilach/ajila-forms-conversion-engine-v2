const BANNED = ["script", "link", "style", "iframe", "object", "embed", "form", "input", "select", "textarea", "button"];

function bannedTags(html) {
  const found = [];
  for (const tag of BANNED) {
    if (new RegExp("<" + tag + "(?=[\\s/>])", "i").test(html) && found.indexOf(tag) < 0) found.push(tag);
  }
  return found;
}

// Every `src="..."` value that is not a data URI, and every `javascript:` href.
function badReferences(html) {
  const found = [];
  const attribute = /\s(src|href)\s*=\s*("([^"]*)"|'([^']*)')/gi;
  let m;
  while ((m = attribute.exec(html)) !== null) {
    const value = (m[3] !== undefined ? m[3] : m[4]).trim();
    if (m[1].toLowerCase() === "src" && !/^data:/i.test(value)) found.push("src=\"" + value.slice(0, 40) + "\"");
    if (m[1].toLowerCase() === "href" && /^javascript:/i.test(value)) found.push("href=\"" + value.slice(0, 40) + "\"");
  }
  return found;
}

function check(output, ctx) {
  const violations = [];
  function visit(node, pointer) {
    if (node.type === "HtmlDisplayer") {
      const content = node.content || {};
      const name = typeof node.name === "string" ? node.name : "";
      for (const lang of Object.keys(content)) {
        const html = String(content[lang]);
        if (html.trim() === "") continue;
        const at = pointer + "/content/" + lang;
        const banned = bannedTags(html);
        if (banned.length > 0) {
          violations.push({
            pointer: at,
            message: "`" + name + "` (" + lang + ") holds " + banned.map(function (t) { return "<" + t + ">"; }).join(", ") +
              ", which does not survive into the printed document: remove it",
          });
        }
        const references = badReferences(html);
        if (references.length > 0) {
          violations.push({
            pointer: at,
            message: "`" + name + "` (" + lang + ") carries " + references.join(", ") +
              ": only a data: URI loads, so embed the image or drop the reference",
          });
        }
        if (name.indexOf("TBL_") === 0 && !(/<table[\s>]/i.test(html) && /<tr[\s>]/i.test(html))) {
          violations.push({
            pointer: at,
            message: "`" + name + "` (" + lang + ") is named for a table but holds no `<table>` with a `<tr>`: " +
              "write the table, or rename the node",
          });
        }
        if (name.indexOf("CRT_") === 0 && !/<svg[\s>]/i.test(html)) {
          violations.push({
            pointer: at,
            message: "`" + name + "` (" + lang + ") is named for a chart but holds no `<svg>`: draw the chart as inline SVG, or rename the node",
          });
        }
        if (name.indexOf("IMG_") === 0 && !/<img[^>]*\ssrc\s*=\s*["']\s*data:/i.test(html)) {
          violations.push({
            pointer: at,
            message: "`" + name + "` (" + lang + ") is named for an image but holds no `<img>` with a data: URI as its `src`: embed the image, or rename the node",
          });
        }
      }
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  return { pass: violations.length === 0, violations: violations };
}
