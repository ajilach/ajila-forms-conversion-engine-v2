// Hides the label of a title-area radio row that the subtitle already prints (UBS-TTL-4). The
// other findings need the source.
function fix(output, ctx) {
  const ops = [];
  for (const v of ctx.violations || []) {
    if (typeof v.pointer === "string" && /\/hide_title$/.test(v.pointer)) {
      ops.push({ op: "add", path: v.pointer, value: true });
    }
  }
  return ops;
}
