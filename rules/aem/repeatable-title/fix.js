// Deletes the headings the check reports (a heading inside a repeatable that names its row, and one
// right before it that is only its title or plural). A missing or plural title needs a person.
function resolve(output, pointer) {
  let node = output;
  const parts = pointer.split("/").slice(1);
  for (const part of parts) {
    if (node === null || typeof node !== "object") return undefined;
    node = node[part.replace(/~1/g, "/").replace(/~0/g, "~")];
  }
  return node;
}

function fix(output, ctx) {
  const pointers = [];
  (ctx.violations || []).forEach(function (v) {
    const node = resolve(output, v.pointer);
    if (node && node.type === "TitleDraw" && /\/children\/[0-9]+$/.test(v.pointer) && pointers.indexOf(v.pointer) < 0) {
      pointers.push(v.pointer);
    }
  });
  // Deepest and last first, so no removal shifts an index still to be removed.
  function key(p) { return p.split("/").filter(function (s) { return /^[0-9]+$/.test(s); }).map(Number); }
  pointers.sort(function (a, b) {
    const x = key(a), y = key(b);
    for (let i = 0; i < Math.min(x.length, y.length); i++) if (x[i] !== y[i]) return y[i] - x[i];
    return y.length - x.length;
  });
  return pointers.map(function (p) { return { op: "remove", path: p }; });
}
