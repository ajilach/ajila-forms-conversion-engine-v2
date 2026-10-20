// The one case a script can settle: a text whose only quotation marks are straight double quotes,
// an even number of them. They pair by position and become the outer pair of the text's language
// and entity (UBS-TXT-2). Anything else (an odd count, a straight quote inside a quotation that
// already has marks, a wrong pair, a straight single quote) needs the sentence read and is left.
const SWISS_ENTITY = "001";

function outerPair(lang, entity) {
  const base = String(lang).toLowerCase().split(/[-_]/)[0];
  const swiss = String(entity === undefined || entity === null ? "" : entity).trim() === SWISS_ENTITY;
  if (base === "de") return swiss ? ["«", "»"] : ["„", "“"];
  if (base === "it") return swiss ? ["„", "“"] : ["«", "»"];
  if (base === "fr") return swiss ? ["«", "»"] : ["« ", " »"];
  if (base === "es" || base === "sp") return ["«", "»"];
  if (base === "en") return ["“", "”"];
  return null;
}

const TAG = /(<\/?[A-Za-z][^>]*>|<!--[\s\S]*?-->)/;
const STRAIGHT = /"|&quot;|&#34;|&#x22;/gi;
// Any other quotation mark, as a character or an entity; `’` is the apostrophe and does not count.
const OTHER = /[«»‹›„“”‚‘‛‟]|&(laquo|raquo|lsaquo|rsaquo|bdquo|ldquo|rdquo|sbquo|lsquo);|&#(171|187|8249|8250|8222|8220|8221|8218|8216);/i;
const OPENING_SINGLE = /(^|[\s(\[{\u2013\u2014\/])('|&#39;|&#x27;|&apos;)(?=[^\s'&])/;

function unescapeToken(token) {
  return token.replace(/~1/g, "/").replace(/~0/g, "~");
}

function at(doc, pointer) {
  let node = doc;
  for (const part of pointer.split("/").slice(1)) {
    if (node === null || typeof node !== "object") return undefined;
    node = node[unescapeToken(part)];
  }
  return node;
}

function rewrite(value, pair) {
  const parts = value.split(TAG);
  let text = "";
  for (let i = 0; i < parts.length; i += 2) text += parts[i];
  const straight = (text.match(STRAIGHT) || []).length;
  if (straight === 0 || straight % 2 === 1 || OTHER.test(text) || OPENING_SINGLE.test(text)) return null;
  let n = 0;
  for (let i = 0; i < parts.length; i += 2) {
    parts[i] = parts[i].replace(STRAIGHT, function () {
      return pair[n++ % 2];
    });
  }
  // A space the source set inside the quotes gives way to the no-break space of the French pair.
  return parts.join("").replace(/«  +/g, "« ").replace(/ + »/g, " »");
}

function fix(output, ctx) {
  const entity = (output.variables || {}).formrange_entity;
  const ops = [];
  const seen = {};
  for (const v of ctx.violations || []) {
    if (seen[v.pointer]) continue;
    seen[v.pointer] = true;
    const value = at(output, v.pointer);
    const lang = unescapeToken(v.pointer.split("/").pop());
    const pair = outerPair(lang, entity);
    if (typeof value !== "string" || pair === null) continue;
    const fixed = rewrite(value, pair);
    if (fixed !== null && fixed !== value) ops.push({ op: "replace", path: v.pointer, value: fixed });
  }
  return ops;
}
