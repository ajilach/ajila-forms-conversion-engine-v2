// Repairs what needs no reading of the source: the Italian branch's registration text missing
// from a form of entity 033, and the flags of a hidden infobox copy. An issuer line drawn in the
// body, and a registration text in the wrong place, are left to the author.
const INFOBOX = /affrg_italy_infobox$/;
const INFOBOX_PATH = "/content/dam/formsanddocuments/afforms_italy_fragmentlib/affrg_italy_infobox";

function fix(output, ctx) {
  const ops = [];
  const top = output.form.children || [];
  let lastPage = -1;
  for (let i = 0; i < top.length; i++) {
    if (top[i].type === "Panel" && top[i].is_page === true) lastPage = i;
  }
  let shown = 0;
  function visit(node, pointer) {
    if (node.type === "Fragment" && typeof node.frag_ref === "string" && INFOBOX.test(node.frag_ref)) {
      if (node.visible === false) {
        if (node.always_in_pdf !== true) ops.push({ op: "add", path: pointer + "/always_in_pdf", value: true });
        if (node.summary_exclude !== true) ops.push({ op: "add", path: pointer + "/summary_exclude", value: true });
        if (node.dor_exclude === true) ops.push({ op: "replace", path: pointer + "/dor_exclude", value: false });
      } else {
        shown += 1;
      }
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  const entity = String((output.variables || {}).formrange_entity || "");
  if (entity === "033" && shown === 0 && lastPage >= 0) {
    ops.push({
      op: "add",
      path: "/form/children/" + lastPage + "/children/-",
      value: {
        type: "Fragment",
        // A fixed uuid, like the writer's own DoR copy: one registration text per form.
        uuid: "5f0c1d2e-3a4b-4c5d-8e6f-7a8b9c0d1e2f",
        name: "PN_ItalyInfobox",
        title: {},
        frag_ref: INFOBOX_PATH,
        visible: true,
        bind_ref: null,
      },
    });
  }
  return ops;
}
