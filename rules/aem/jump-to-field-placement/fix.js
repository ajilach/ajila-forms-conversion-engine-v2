// Every finding is an authored `jump_to_field` the writer's own placement makes wrong or useless:
// delete it.
function fix(output, ctx) {
  const seen = [];
  (ctx.violations || []).forEach(function (v) {
    if (/\/jump_to_field$/.test(v.pointer) && seen.indexOf(v.pointer) < 0) seen.push(v.pointer);
  });
  return seen.map(function (p) { return { op: "remove", path: p }; });
}
