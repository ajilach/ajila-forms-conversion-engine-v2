// Repairs every notices finding: a labelled paragraph becomes a MessageBox (a TextDraw has every
// field a MessageBox has), a repeated notice is removed, an ignored print flag is dropped.
function fix(output, ctx) {
  const ops = [];
  const removals = [];
  for (const v of ctx.violations || []) {
    const last = v.pointer.split("/").pop();
    if (last === "type") ops.push({ op: "replace", path: v.pointer, value: "MessageBox" });
    else if (last === "always_in_pdf" || last === "show_if_hidden") ops.push({ op: "remove", path: v.pointer });
    else removals.push(v.pointer);
  }
  // Last first, so that a removal does not move the nodes of the ones still to come.
  function indices(pointer) {
    return pointer.split("/").filter(function (p) { return /^[0-9]+$/.test(p); }).map(Number);
  }
  removals.sort(function (a, b) {
    const x = indices(a);
    const y = indices(b);
    for (let i = 0; i < Math.min(x.length, y.length); i++) if (x[i] !== y[i]) return y[i] - x[i];
    return y.length - x.length;
  });
  removals.forEach(function (pointer) { ops.push({ op: "remove", path: pointer }); });
  return ops;
}
